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
use crate::g0_workflow::{
    derive_workflow_plan, derive_workflow_plan_with_context, DerivedWorkflowPlan,
    WorkflowDerivationContext,
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clap::Args;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
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
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_HOST: &str = "api.github.com";
/// Bound caller-supplied YAML and base64 allocation before parsing or hashing.
const G0_MAX_WORKFLOW_SOURCE_BYTES: usize = 1024 * 1024;
const G0_MAX_WORKFLOW_SOURCE_BASE64_BYTES: usize = G0_MAX_WORKFLOW_SOURCE_BYTES.div_ceil(3) * 4;
/// Bound caller-supplied REST/GraphQL query components before base64 decoding.
/// GraphQL remains unsupported until its operation and purpose are authorized.
const G0_MAX_REQUEST_FIELD_BYTES: usize = 1024 * 1024;
const G0_MAX_REQUEST_FIELD_BASE64_BYTES: usize = G0_MAX_REQUEST_FIELD_BYTES.div_ceil(3) * 4;
/// Raw API bodies are diagnostic evidence only until exact response rows are
/// authenticated. Bound both encoded and decoded bodies before allocation.
const G0_MAX_RAW_OBJECT_BYTES: usize = G0_MAX_REQUEST_FIELD_BYTES;
const G0_MAX_RAW_OBJECT_BASE64_BYTES: usize = G0_MAX_RAW_OBJECT_BYTES.div_ceil(3) * 4;
const G0_GENERATED_STATE_SCHEMA: &str = "velnor.generated-state.v1";
const G0_GENERATED_STATE_ARCHIVE_MEMBER: &str = "generated-state.json";
const G0_ALLOWED_SCOPES: [&str; 7] = [
    "actions:read",
    "administration:read",
    "checks:read",
    "contents:read",
    "metadata:read",
    "pull_requests:read",
    "statuses:read",
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
    pub child_workflows: Vec<ChildWorkflowObservation>,
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
/// Caller-supplied workflow membership. A `workflow_call` shares its caller's
/// run ID and attempt; `workflow_run` and `workflow_dispatch` create a distinct
/// workflow run. None of these rows authenticate the child target by itself.
pub(crate) struct ChildWorkflowObservation {
    pub parent_run_id: u64,
    pub parent_run_attempt: u32,
    pub parent_repository: String,
    pub parent_workflow_path: String,
    pub parent_source_sha: String,
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
    pub child_workflow_links: Vec<ChildWorkflowLink>,
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
/// Caller-supplied projection of a workflow membership row.
pub(crate) struct ChildWorkflowLink {
    pub parent_run_id: u64,
    pub parent_run_attempt: u32,
    pub parent_repository: String,
    pub parent_workflow_path: String,
    pub parent_source_sha: String,
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

fn g0_child_parent_run_repository(
    child: &ChildWorkflowObservation,
    root_repository: &str,
    execution: &ExecutionObservation,
    remaining_depth: usize,
) -> Option<String> {
    if remaining_depth == 0 {
        return None;
    }
    if child.parent_run_id == execution.run_id
        && child.parent_run_attempt == execution.run_attempt
        && child.parent_repository == root_repository
        && child.parent_workflow_path == execution.workflow_path
        && child.parent_source_sha == execution.workflow_revision
    {
        return Some(root_repository.to_owned());
    }

    let mut parents = execution.child_workflows.iter().filter(|parent| {
        parent.run_id == child.parent_run_id
            && parent.run_attempt == child.parent_run_attempt
            && parent.repository == child.parent_repository
            && parent.workflow_path == child.parent_workflow_path
            && parent.source_sha == child.parent_source_sha
    });
    let parent = parents.next()?;
    if parents.next().is_some() {
        return None;
    }
    if parent.event == "workflow_call" {
        if parent.run_id != parent.parent_run_id || parent.run_attempt != parent.parent_run_attempt
        {
            return None;
        }
        g0_child_parent_run_repository(parent, root_repository, execution, remaining_depth - 1)
    } else {
        Some(parent.repository.clone())
    }
}

fn g0_child_workflow_run_identity_is_valid(
    child: &ChildWorkflowObservation,
    root_repository: &str,
    execution: &ExecutionObservation,
) -> bool {
    let Some(parent_run_repository) = g0_child_parent_run_repository(
        child,
        root_repository,
        execution,
        execution.child_workflows.len() + 1,
    ) else {
        return false;
    };
    if child.event == "workflow_call" {
        child.run_id == child.parent_run_id && child.run_attempt == child.parent_run_attempt
    } else {
        !child
            .repository
            .eq_ignore_ascii_case(&parent_run_repository)
            || child.run_id != child.parent_run_id
    }
}

fn g0_child_workflow_run_url_is_valid(
    child: &ChildWorkflowObservation,
    root_repository: &str,
    execution: &ExecutionObservation,
) -> bool {
    let run_repository = if child.event == "workflow_call" {
        let Some(parent_run_repository) = g0_child_parent_run_repository(
            child,
            root_repository,
            execution,
            execution.child_workflows.len() + 1,
        ) else {
            return false;
        };
        parent_run_repository
    } else {
        child.repository.clone()
    };
    run_url_matches_repository(&child.source_url, &run_repository, child.run_id)
}

fn g0_run_identity_key(repository: &str, run_id: u64, run_attempt: u32) -> (String, u64, u32) {
    (repository.to_ascii_lowercase(), run_id, run_attempt)
}

fn g0_release_producer_is_record_run(
    producer: &ReleaseEvidenceProducer,
    record: &EvidenceRecord,
) -> bool {
    producer.repository == record.repository
        && producer.workflow_path == record.workflow_path
        && producer.run_id == record.run_id
        && producer.run_url == record.run_url
        && producer.source_commit == record.actual_checkout_sha
}

fn g0_release_producer_matches_child_link(
    producer: &ReleaseEvidenceProducer,
    link: &ChildWorkflowLink,
) -> bool {
    producer.repository == link.repository
        && producer.workflow_path == link.workflow_path
        && producer.run_id == link.run_id
        && producer.run_url == link.run_url
        && producer.source_commit == link.source_sha
}

fn g0_child_workflow_observation_identity(
    child: &ChildWorkflowObservation,
) -> (u64, u32, &str, &str, &str, u64, u32, &str, &str, &str, &str) {
    (
        child.parent_run_id,
        child.parent_run_attempt,
        &child.parent_repository,
        &child.parent_workflow_path,
        &child.parent_source_sha,
        child.run_id,
        child.run_attempt,
        &child.repository,
        &child.workflow_path,
        &child.event,
        &child.source_sha,
    )
}

fn g0_child_workflow_link_identity(
    child: &ChildWorkflowLink,
) -> (u64, u32, &str, &str, &str, u64, u32, &str, &str, &str, &str) {
    (
        child.parent_run_id,
        child.parent_run_attempt,
        &child.parent_repository,
        &child.parent_workflow_path,
        &child.parent_source_sha,
        child.run_id,
        child.run_attempt,
        &child.repository,
        &child.workflow_path,
        &child.event,
        &child.source_sha,
    )
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
    let (manifest, snapshot, evidence, release, raw_store) = capture.into_parts();
    if let Some(inventory) = evidence.g0_inventory.as_ref() {
        raw_store
            .verify_g0(inventory)
            .context("verify producer raw-store binding")?;
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
    let mut workflow_derivation = WorkflowDerivationContext::default();
    check_headers(stage, manifest, snapshot, evidence, &mut findings);
    check_manifest(manifest, &mut findings);
    check_snapshot(manifest, snapshot, &mut findings);
    if mode == CheckMode::TrustedLive {
        finding(
            &mut findings,
            "manifest-projection-unverified",
            "",
            "manifest.source/repositories",
            "no reviewed enriched manifest bytes are bound to the pinned source revision; caller-provided workload projections cannot authorize a live gate",
        );
        finding(
            &mut findings,
            "check-id-response-unverified",
            "",
            "snapshot.required_checks/evidence.required_checks",
            "check-suite, check-run, and job IDs are not bound to parsed provider response rows",
        );
        if stage == Stage::G0 {
            for (code, field, message) in [
                (
                    "g0-api-row-response-unverified",
                    "evidence.g0_inventory.collector_snapshot",
                    "repository, PR, ruleset, check, and access claims are not all parsed from subject-specific raw API responses",
                ),
                (
                    "g0-request-api-version-unverified",
                    "evidence.g0_inventory.collector_snapshot.requests",
                    "request and response API-version headers are not captured and joined to each request; collector.api_versions alone is caller-authored metadata",
                ),
                (
                    "g0-check-id-response-unverified",
                    "evidence.g0_inventory.collector_snapshot.check_producers",
                    "check-suite, check-run, and job IDs are not bound to parsed provider response rows",
                ),
                (
                    "g0-artifact-row-unverified",
                    "evidence.g0_inventory.collector_snapshot.repositories.artifacts",
                    "artifact metadata rows are not parsed and joined to the captured artifact-list response",
                ),
                (
                    "g0-artifact-attempt-unverified",
                    "evidence.g0_inventory.collector_snapshot.repositories.artifacts",
                    "the artifact-list response does not expose a run attempt, so attempt identity is not independently proven",
                ),
                (
                    "g0-artifact-archive-unverified",
                    "evidence.g0_inventory.collector_snapshot.repositories.artifacts",
                    "the reported artifact digest is not verified against downloaded archive bytes",
                ),
                (
                    "g0-artifact-source-digest-unverified",
                    "evidence.g0_inventory.collector_snapshot.workload_artifact.source_digest",
                    "the source digest is not joined to captured source bytes, revision, and URL",
                ),
                (
                    "g0-pagination-response-unverified",
                    "evidence.g0_inventory.collector_snapshot.requests.page",
                    "page completeness, item counts, and next links are caller metadata rather than parsed provider response headers and rows",
                ),
                (
                    "required-check-inventory-unverified",
                    "evidence.g0_inventory.collector_snapshot.repositories.rulesets",
                    "ruleset rows do not prove active/default-branch applicability, and classic branch-protection required checks with app IDs are not collected",
                ),
                (
                    "g0-child-target-binding-unverified",
                    "evidence.g0_inventory.collector_snapshot.dependency_graph",
                    "source SHAs and caller-provided child links do not authenticate the exact runtime child repository/workflow target",
                ),
            ] {
                finding(&mut findings, code, "", field, message);
            }
        }
        if stage.needs_release() {
            finding(
                &mut findings,
                "g0-release-producer-unverified",
                "",
                "release_manifest/evidence.records.release",
                "release documents and producer claims are caller supplied and are not bound to authenticated provider responses or immutable producer bytes",
            );
        }
        if stage.needs_execution() {
            finding(
                &mut findings,
                "g0-child-target-binding-unverified",
                "",
                "evidence.g0_inventory.collector_snapshot.dependency_graph/evidence.records.child_workflow_links",
                "caller-provided child workflow associations do not authenticate the exact runtime child repository and workflow target",
            );
        }
    }
    if stage == Stage::G0 {
        check_g0_inventory_with_context(
            manifest,
            snapshot,
            evidence.g0_inventory.as_ref(),
            &mut workflow_derivation,
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
                &mut workflow_derivation,
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
    if !g0_api_base(&snapshot.source.api_base) {
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
    if !g0_scope_list_is_allowed(&snapshot.source.permission_scopes) {
        finding(
            findings,
            "snapshot-access",
            "",
            "snapshot.source.permission_scopes",
            "collector must record unique supported read-only GitHub scopes",
        );
    }
    let mut seen = BTreeSet::new();
    let mut execution_identities = BTreeSet::new();
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

        for execution in repo.main_executions.iter().chain(
            repo.open_prs
                .iter()
                .flat_map(|pull_request| pull_request.executions.iter()),
        ) {
            if !execution_identities.insert(g0_run_identity_key(
                &repo.repository,
                execution.run_id,
                execution.run_attempt,
            )) {
                finding(
                    findings,
                    "snapshot-duplicate-execution",
                    &repo.repository,
                    "main_executions/open_prs.executions",
                    "run ID/attempt identities must be globally unique across main and PR lanes",
                );
            }
            for child in &execution.child_workflows {
                if child.event == "workflow_call" {
                    // Reusable workflows execute inside the caller's run and
                    // attempt, so they are workflow memberships, not new runs.
                    continue;
                }
                if !execution_identities.insert(g0_run_identity_key(
                    &child.repository,
                    child.run_id,
                    child.run_attempt,
                )) {
                    finding(
                        findings,
                        "snapshot-duplicate-execution",
                        &repo.repository,
                        "main_executions/open_prs.executions/child_workflows",
                        "distinct child workflow runs must be unique across main and PR lanes",
                    );
                }
            }
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
    let mut workflow_derivation = WorkflowDerivationContext::default();
    check_g0_inventory_with_context(
        manifest,
        snapshot,
        inventory,
        &mut workflow_derivation,
        findings,
    );
}

fn check_g0_inventory_with_context(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    inventory: Option<&G0InventoryEvidence>,
    workflow_derivation: &mut WorkflowDerivationContext,
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
    let workflow_sources_bounded = collector.repositories.iter().all(|repository| {
        repository
            .workflows
            .iter()
            .all(g0_workflow_inventory_sources_size_within_limit)
    });
    if !workflow_sources_bounded {
        finding(
            findings,
            "g0-workflow-source",
            "",
            "evidence.g0_inventory.collector_snapshot.repositories.workflows",
            "workflow source and dependencies must stay within the one MiB checker input limit",
        );
    }
    let request_fields_bounded = collector.requests.iter().all(|request| {
        g0_request_field_size_within_limit(&request.query_base64)
            && g0_request_field_size_within_limit(&request.variables_base64)
    });
    if !request_fields_bounded {
        finding(
            findings,
            "g0-request-size",
            "",
            "evidence.g0_inventory.collector_snapshot.requests.query_base64/variables_base64",
            "request query and variable fields must stay within the one MiB encoded and decoded checker limits before snapshot serialization",
        );
    }
    let raw_objects_bounded = collector
        .raw_objects
        .iter()
        .all(g0_raw_object_bytes_within_limit);
    if !raw_objects_bounded {
        finding(
            findings,
            "g0-raw-object-size",
            "",
            "evidence.g0_inventory.collector_snapshot.raw_objects.bytes_base64",
            "raw object bodies must stay within the one MiB encoded and decoded checker limits before snapshot serialization",
        );
    }
    let snapshot_material_bounded =
        workflow_sources_bounded && request_fields_bounded && raw_objects_bounded;
    let expected_value =
        snapshot_material_bounded.then(|| serde_json::to_value(collector).unwrap_or(Value::Null));
    let expected_bytes = expected_value
        .as_ref()
        .map(|value| canonical_json(value).into_bytes());
    let encoded_snapshot_within_limit = expected_bytes.as_ref().is_some_and(|bytes| {
        let max_encoded_length = bytes.len().div_ceil(3).saturating_mul(4);
        inventory.collector_snapshot_bytes_base64.len() <= max_encoded_length
    });
    let decoded_snapshot = encoded_snapshot_within_limit
        .then(|| {
            BASE64
                .decode(&inventory.collector_snapshot_bytes_base64)
                .ok()
        })
        .flatten();
    let expected_digest = decoded_snapshot
        .as_ref()
        .map(|bytes| digest_bytes(bytes))
        .unwrap_or_default();
    let snapshot_bytes_valid = match (
        decoded_snapshot.as_ref(),
        expected_bytes.as_ref(),
        expected_value.as_ref(),
    ) {
        (Some(bytes), Some(expected_bytes), Some(expected_value)) => {
            reject_duplicate_json_keys(bytes).is_ok()
                && bytes == expected_bytes
                && serde_json::from_slice::<G0CollectorSnapshot>(bytes)
                    .ok()
                    .and_then(|parsed| serde_json::to_value(parsed).ok())
                    .is_some_and(|parsed| canonical_json(&parsed) == canonical_json(expected_value))
        }
        _ => false,
    };
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
    check_g0_repositories(
        manifest,
        snapshot,
        collector,
        &raw_ids,
        workflow_derivation,
        findings,
    );
    check_g0_reconciliation(manifest, snapshot, collector, &raw_ids, findings);
    check_g0_dependency_graph(manifest, collector, &raw_ids, workflow_derivation, findings);
    check_g0_model_session(collector, &raw_ids, findings);
    check_g0_access(collector, findings);
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
        || !g0_scope_list_is_allowed(&collector.auth.safe_scopes)
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

fn g0_scope_list_is_allowed(scopes: &[String]) -> bool {
    let unique = scopes.iter().map(String::as_str).collect::<BTreeSet<_>>();
    !scopes.is_empty()
        && unique.len() == scopes.len()
        && scopes
            .iter()
            .all(|scope| G0_ALLOWED_SCOPES.contains(&scope.as_str()))
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
        let query = decode_g0_request_field(&request.query_base64);
        let variables = decode_g0_request_field(&request.variables_base64);
        if request.request_id.trim().is_empty()
            || request.method.trim().is_empty()
            || request.endpoint_or_operation.trim().is_empty()
            || request.accept.trim().is_empty()
            || query.is_none()
            || variables.is_none()
            || query
                .as_ref()
                .is_some_and(|bytes| digest_bytes(bytes) != request.query_sha256)
            || variables
                .as_ref()
                .is_some_and(|bytes| digest_bytes(bytes) != request.variables_sha256)
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
            || request
                .page
                .per_page
                .is_some_and(|per_page| per_page == 0 || per_page > 100)
            || request
                .page
                .per_page
                .is_some_and(|per_page| request.page.items_returned > per_page)
            || request.response_raw_ref.trim().is_empty()
            || request.error_raw_ref.is_some()
            || !request.complete
            || request.truncation_reason.is_some()
            || !matches!(
                request.state,
                G0RequestState::Complete | G0RequestState::EmptyComplete
            )
            || (request.state == G0RequestState::EmptyComplete && request.page.items_returned != 0)
            || (request.page.has_next_page && request.page.link_next.is_none())
            || (request.page.has_next_page
                && request.page.link_next.as_ref().is_some_and(|link| {
                    !g0_api_url(link).is_some_and(|url| url.path() == request.endpoint_or_operation)
                }))
            || (!request.page.has_next_page
                && (request.page.link_next.is_some()
                    || request.page.cursor_in.is_some()
                    || request.page.cursor_out.is_some()))
            || !g0_request_semantics(request, query.as_deref(), variables.as_deref())
        {
            finding(
                findings,
                "g0-request-incomplete",
                "",
                "evidence.g0_inventory.collector_snapshot.requests",
                "every request must be a complete successful page with query, viewer, rate-limit, and raw-response provenance",
            );
        }
    }
    let mut pages = BTreeMap::<(G0ApiKind, String, String, String), Vec<&G0RequestRecord>>::new();
    for request in &collector.requests {
        let query = decode_g0_request_field(&request.query_base64);
        let variables = decode_g0_request_field(&request.variables_base64);
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
    for raw in &collector.raw_objects {
        let decoded = decode_g0_raw_object_bytes(raw);
        if !raw_ids.insert(raw.raw_id.clone())
            || !request_ids.contains(&raw.request_id)
            || raw.object_kind.trim().is_empty()
            || raw.canonicalization.trim().is_empty()
            || !valid_digest(&raw.sha256)
            || raw.byte_length == 0
            || decoded.is_none()
            || decoded
                .as_ref()
                .is_some_and(|bytes| bytes.len() as u64 != raw.byte_length)
            || decoded
                .as_ref()
                .is_some_and(|bytes| digest_bytes(bytes) != raw.sha256)
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

fn check_g0_generated_state_reference(
    repository: &str,
    workflow: &G0WorkflowInventory,
    inventory: &G0RepositoryInventory,
    snapshot_repository: Option<&SnapshotRepository>,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    let artifact = &workflow.generated_state;
    let source = &workflow.source;
    let field = "evidence.g0_inventory.collector_snapshot.workflow.generated_state";
    if artifact.schema != G0_GENERATED_STATE_SCHEMA
        || artifact.source_url != source.source_url
        || artifact.source_revision != source.revision
        || artifact.source_digest != source.sha256
    {
        finding(
            findings,
            "g0-generated-state-source",
            repository,
            field,
            "generated-state source URL, revision, and digest must identify this workflow's captured immutable source bytes",
        );
    }

    let matching_artifacts = inventory
        .artifacts
        .iter()
        .filter(|candidate| {
            candidate.name == artifact.name
                && candidate.digest == artifact.sha256
                && !candidate.expired
                && g0_artifact_source_url(repository, candidate.artifact_id, &candidate.source_url)
        })
        .collect::<Vec<_>>();
    let [artifact_row] = matching_artifacts.as_slice() else {
        finding(
            findings,
            "g0-generated-state-artifact",
            repository,
            field,
            "generated-state must identify exactly one non-expired captured workflow artifact row by name and measured digest",
        );
        return;
    };

    let referenced_raw = artifact
        .raw_object_refs
        .iter()
        .filter_map(|raw_id| raw_objects.iter().find(|raw| raw.raw_id == *raw_id))
        .filter(|raw| {
            raw.object_kind == "workflow_artifact_archive" && raw.sha256 == artifact.sha256
        })
        .collect::<Vec<_>>();
    let [raw] = referenced_raw.as_slice() else {
        finding(
            findings,
            "g0-generated-state-artifact",
            repository,
            field,
            "generated-state must reference exactly one measured workflow-artifact archive, not an unrelated API response with the same digest field",
        );
        return;
    };

    let matching_requests = requests
        .iter()
        .filter(|request| {
            request.request_id == raw.request_id && request.response_raw_ref == raw.raw_id
        })
        .collect::<Vec<_>>();
    let [request] = matching_requests.as_slice() else {
        finding(
            findings,
            "g0-generated-state-artifact",
            repository,
            field,
            "generated-state archive bytes must be the response to one captured artifact-download request",
        );
        return;
    };
    let expected_endpoint = format!(
        "/repos/{repository}/actions/artifacts/{}/zip",
        artifact_row.artifact_id
    );
    let archive_bytes = decode_g0_raw_object_bytes(raw);
    if artifact.raw_object_refs.len() != 1
        || artifact_row.run_id == 0
        || artifact_row.run_head_sha.trim().is_empty()
        || artifact_row.digest != artifact.sha256
        || artifact_row.name != artifact.name
        || artifact_row.source_url != format!("{GITHUB_API_BASE}{expected_endpoint}")
        || request.api != G0ApiKind::Rest
        || request.method != "GET"
        || request.endpoint_or_operation != expected_endpoint
        || request.http_status != 200
        || request.response_raw_ref != raw.raw_id
        || !request.complete
        || request.truncation_reason.is_some()
        || !matches!(
            request.state,
            G0RequestState::Complete | G0RequestState::EmptyComplete
        )
        || raw.media_type != "application/zip"
        || raw.canonicalization != "identity"
        || raw.original_sha256 != raw.sha256
        || raw.original_byte_length != raw.byte_length
        || archive_bytes
            .as_ref()
            .is_none_or(|bytes| digest_bytes(bytes) != artifact.sha256)
    {
        finding(
            findings,
            "g0-generated-state-artifact",
            repository,
            field,
            "generated-state archive kind, exact download endpoint, raw bytes, provider artifact row, and measured digest must agree",
        );
        return;
    }

    let matching_runs = snapshot_repository
        .into_iter()
        .flat_map(|snapshot| {
            snapshot.main_executions.iter().chain(
                snapshot
                    .open_prs
                    .iter()
                    .flat_map(|pull_request| pull_request.executions.iter()),
            )
        })
        .filter(|execution| {
            execution.run_id == artifact_row.run_id
                && execution.trigger_source_sha == artifact_row.run_head_sha
                && execution.workflow_path == source.path
                && execution.workflow_revision == source.revision
                && execution.status == "completed"
                && execution.conclusion == "success"
        })
        .collect::<Vec<_>>();
    if matching_runs.len() != 1 {
        finding(
            findings,
            "g0-generated-state-run",
            repository,
            field,
            "generated-state artifact run must uniquely match a successful snapshot execution of its captured workflow source and run head",
        );
    }

    if !archive_bytes
        .as_deref()
        .is_some_and(|bytes| g0_generated_state_archive_matches(bytes, artifact))
    {
        finding(
            findings,
            "g0-generated-state-schema",
            repository,
            field,
            "generated-state archive must contain one canonical generated-state JSON document with the declared schema and matching source URL, revision, and digest",
        );
    }
}

fn g0_generated_state_archive_matches(bytes: &[u8], artifact: &G0ArtifactReference) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    if archive
        .file_names()
        .filter(|name| *name == G0_GENERATED_STATE_ARCHIVE_MEMBER)
        .count()
        != 1
    {
        return false;
    }
    let Ok(mut file) = archive.by_name(G0_GENERATED_STATE_ARCHIVE_MEMBER) else {
        return false;
    };
    if file.size() > G0_MAX_RAW_OBJECT_BYTES as u64 {
        return false;
    }
    let expected_size = file.size();
    let mut payload_bytes = Vec::with_capacity(expected_size as usize);
    if file.read_to_end(&mut payload_bytes).is_err() || payload_bytes.len() as u64 != expected_size
    {
        return false;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&payload_bytes) else {
        return false;
    };
    if canonical_json(&payload).as_bytes() != payload_bytes.as_slice() {
        return false;
    }
    payload.get("schema").and_then(Value::as_str) == Some(artifact.schema.as_str())
        && payload.get("source_url").and_then(Value::as_str) == Some(artifact.source_url.as_str())
        && payload.get("source_revision").and_then(Value::as_str)
            == Some(artifact.source_revision.as_str())
        && payload.get("source_digest").and_then(Value::as_str)
            == Some(artifact.source_digest.as_str())
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

fn g0_rest_request_repository(endpoint: &str) -> Option<String> {
    let repository_path = endpoint.strip_prefix("/repos/")?;
    let (owner, repository_path) = repository_path.split_once('/')?;
    let repository = repository_path.split('/').next()?;
    if owner.trim().is_empty() || repository.trim().is_empty() {
        return None;
    }
    Some(format!("{owner}/{repository}"))
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
    if endpoint.contains("://")
        || endpoint.contains('?')
        || request.accept.trim().is_empty()
        || request.accept.contains('\r')
        || request.accept.contains('\n')
    {
        return false;
    }
    match request.api {
        G0ApiKind::Rest => {
            if request.method != "GET"
                || !(endpoint == "/user"
                    || endpoint == "/rate_limit"
                    || endpoint.starts_with("/repos/")
                    || endpoint.starts_with("/orgs/"))
                || !g0_rest_endpoint_path_is_safe(endpoint)
            {
                return false;
            }
            if let Some(repository_path) = endpoint.strip_prefix("/repos/") {
                let parts = repository_path.split('/').collect::<Vec<_>>();
                if parts.len() < 2
                    || parts[0].trim().is_empty()
                    || parts[1].trim().is_empty()
                    || !canonical_scope().contains(&format!("{}/{}", parts[0], parts[1]).as_str())
                {
                    return false;
                }
                let repository = format!("{}/{}", parts[0], parts[1]);
                if g0_repository_request_scopes(request, &repository).is_none() {
                    return false;
                }
            } else if !matches!(endpoint, "/user" | "/rate_limit") {
                // G0 has no organization endpoint purpose or response model.
                // Keep the two collector-wide identity/rate endpoints exact.
                return false;
            }
            let Some(pairs) = g0_rest_query_pairs(query) else {
                return false;
            };
            if matches!(endpoint, "/user" | "/rate_limit") && !pairs.is_empty() {
                return false;
            }
            if !g0_rest_page_matches_query(request, &pairs) {
                return false;
            }
            if pairs
                .iter()
                .any(|(key, value)| !g0_rest_query_parameter_is_authorized(endpoint, key, value))
            {
                return false;
            }
            variables == b"{}" || variables.is_empty()
        }
        G0ApiKind::Graphql => {
            // Schema v2 has no allowlisted GraphQL operation/variable pairs
            // bound to a specific inventory purpose. Reject all GraphQL until
            // that authority is represented explicitly.
            false
        }
    }
}

fn g0_rest_query_parameter_is_authorized(endpoint: &str, key: &str, value: &str) -> bool {
    let parts = endpoint.split('/').skip(1).collect::<Vec<_>>();
    match key {
        "page" | "per_page" => g0_rest_endpoint_supports_pagination(endpoint),
        "ref" => matches!(parts.as_slice(), ["repos", _, _, "contents", ..]) && valid_sha(value),
        "state" => matches!(parts.as_slice(), ["repos", _, _, "pulls"]) && value == "open",
        _ => false,
    }
}

fn g0_rest_page_matches_query(request: &G0RequestRecord, pairs: &[(String, String)]) -> bool {
    let endpoint = request.endpoint_or_operation.as_str();
    let supports_pagination = g0_rest_endpoint_supports_pagination(endpoint);
    let page_query = pairs.iter().find(|(key, _)| key == "page");
    let per_page_query = pairs.iter().find(|(key, _)| key == "per_page");
    let query_page = match page_query {
        Some((_, value)) => value.parse::<u32>().ok(),
        None => Some(1),
    };
    if !supports_pagination {
        return page_query.is_none()
            && per_page_query.is_none()
            && request.page.number == 1
            && request.page.per_page.is_none()
            && !request.page.has_next_page
            && request.page.link_next.is_none()
            && request.page.cursor_in.is_none()
            && request.page.cursor_out.is_none();
    }
    let Some((_, per_page)) = per_page_query else {
        return false;
    };
    let Some(query_page) = query_page else {
        return false;
    };
    let Ok(query_per_page) = per_page.parse::<u32>() else {
        return false;
    };
    query_page > 0
        && request.page.number == query_page
        && request.page.per_page == Some(query_per_page)
        && request.page.cursor_in.is_none()
        && request.page.cursor_out.is_none()
        && (1..=100).contains(&query_per_page)
}

fn g0_rest_endpoint_supports_pagination(endpoint: &str) -> bool {
    let parts = endpoint.split('/').skip(1).collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["repos", _, _, "actions", "runs"]
            | ["repos", _, _, "actions", "runs", _, "artifacts"]
            | ["repos", _, _, "actions", "runs", _, "jobs"]
            | ["repos", _, _, "actions", "runs", _, "attempts", _, "jobs"]
            | ["repos", _, _, "actions", "runners"]
            | ["repos", _, _, "actions", "workflows"]
            | ["repos", _, _, "actions", "workflows", _, "runs"]
            | ["repos", _, _, "pulls"]
            | ["repos", _, _, "pulls", _, "commits"]
            | ["repos", _, _, "pulls", _, "files"]
            | ["repos", _, _, "rules", "branches", _]
            | ["repos", _, _, "rulesets"]
            | ["repos", _, _, "rulesets", "rule-suites"]
            | ["repos", _, _, "commits", _, "statuses"]
            | ["repos", _, _, "commits", _, "check-runs"]
            | ["repos", _, _, "commits", _, "check-suites"]
            | ["repos", _, _, "check-suites", _, "check-runs"]
    )
}

fn g0_rest_endpoint_path_is_safe(endpoint: &str) -> bool {
    if !endpoint.starts_with('/') || endpoint.starts_with("//") {
        return false;
    }
    let Ok(url) = Url::parse(&format!("{GITHUB_API_BASE}{endpoint}")) else {
        return false;
    };
    if url.path() != endpoint || url.query().is_some() || url.fragment().is_some() {
        return false;
    }
    let Some("") = endpoint.split('/').next() else {
        return false;
    };
    endpoint.split('/').skip(1).all(|component| {
        if component.is_empty() {
            return false;
        }
        let Some(decoded) = g0_percent_decode_path_component(component) else {
            return false;
        };
        !matches!(decoded.as_slice(), b"." | b"..")
            && !decoded.iter().any(|byte| matches!(byte, b'/' | b'\\'))
    })
}

fn g0_percent_decode_path_component(component: &str) -> Option<Vec<u8>> {
    let encoded = component.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] != b'%' {
            decoded.push(encoded[index]);
            index += 1;
            continue;
        }
        let high = g0_hex_nibble(*encoded.get(index + 1)?)?;
        let low = g0_hex_nibble(*encoded.get(index + 2)?)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    Some(decoded)
}

fn g0_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn g0_rest_query_pairs(query: &[u8]) -> Option<Vec<(String, String)>> {
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
    pairs.sort();
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
            let stable_query = g0_rest_query_pairs(query)?
                .into_iter()
                .filter(|(key, _)| key != "page" && key != "after")
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            Some((
                G0ApiKind::Rest,
                request.endpoint_or_operation.clone(),
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
        || url.host_str() != Some(GITHUB_API_HOST)
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
        if url.path() != next.endpoint_or_operation {
            return false;
        }
        let Some(next_query_bytes) = decode_g0_request_field(&next.query_base64) else {
            return false;
        };
        let Some(next_query) = g0_rest_query_pairs(&next_query_bytes) else {
            return false;
        };
        let link_query = g0_rest_query_pairs(url.query().unwrap_or_default().as_bytes());
        link_query == Some(next_query)
    } else {
        false
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

fn check_run_url_matches_repository(value: &str, repository: &str, check_run_id: u64) -> bool {
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
        && url.path() == format!("/{repository}/runs/{check_run_id}")
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
        && url.path() == format!("/{repository}/runs/{workflow_run_id}/jobs/{job_id}")
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

struct G0WorkflowSourceContext<'a> {
    repository: &'a str,
    default_branch_sha: &'a str,
    dependencies: &'a [G0WorkflowDependency],
    raw_objects: &'a [G0RawObjectRef],
    requests: &'a [G0RequestRecord],
}

fn check_g0_workflow_source(
    field: &str,
    source: &G0WorkflowSource,
    context: G0WorkflowSourceContext<'_>,
    findings: &mut Vec<Finding>,
) -> Option<DerivedWorkflowPlan> {
    let mut workflow_derivation = WorkflowDerivationContext::default();
    check_g0_workflow_source_with_context(
        field,
        source,
        context,
        &mut workflow_derivation,
        findings,
    )
}

fn check_g0_workflow_source_with_context(
    field: &str,
    source: &G0WorkflowSource,
    context: G0WorkflowSourceContext<'_>,
    workflow_derivation: &mut WorkflowDerivationContext,
    findings: &mut Vec<Finding>,
) -> Option<DerivedWorkflowPlan> {
    let G0WorkflowSourceContext {
        repository,
        default_branch_sha,
        dependencies,
        raw_objects,
        requests,
    } = context;
    let raw_ids = raw_objects
        .iter()
        .map(|raw| raw.raw_id.clone())
        .collect::<BTreeSet<_>>();
    let decoded = decode_g0_workflow_source_bytes(&source.bytes_base64, source.byte_length);
    let valid = !source.repository.trim().is_empty()
        && source.repository == repository
        && g0_workflow_file_path(&source.path)
        && valid_sha(&source.revision)
        && valid_sha(&source.source_sha)
        && source.revision == source.source_sha
        && source.source_sha == default_branch_sha
        && workflow_source_url_matches(source)
        && matches!(source.media_type.as_str(), "text/yaml" | "application/yaml")
        && source.canonicalization == "raw-utf8"
        && valid_digest(&source.sha256)
        && g0_storage_ref(&source.storage_ref, &source.sha256)
        && decoded
            .as_ref()
            .is_some_and(|bytes| digest_bytes(bytes) == source.sha256)
        && !source.raw_object_refs.is_empty()
        && source
            .raw_object_refs
            .iter()
            .all(|raw_id| raw_ids.contains(raw_id));
    let dependency_sizes_valid = dependencies.iter().all(|dependency| {
        g0_workflow_source_size_within_limit(
            &dependency.source.bytes_base64,
            dependency.source.byte_length,
        )
    });
    let raw_binding = source_has_raw_binding(source, raw_objects, requests);
    if !valid || !dependency_sizes_valid || !raw_binding {
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
    match derive_workflow_plan_with_context(source, dependencies, workflow_derivation) {
        Ok(plan) => {
            if plan.has_action_steps {
                finding(
                    findings,
                    "g0-action-semantics-unverified",
                    repository,
                    field,
                    "workflow action steps are source-bound, but action execution and transitive composite dependencies are not derived",
                );
            }
            Some(plan)
        }
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
    g0_github_blob_path(source).is_some_and(|expected_path| url.path() == expected_path)
}

fn g0_github_blob_path(source: &G0WorkflowSource) -> Option<String> {
    let repository_parts = source.repository.split('/').collect::<Vec<_>>();
    if repository_parts.len() != 2
        || repository_parts.iter().any(|part| part.is_empty())
        || !g0_relative_source_path_is_safe(&source.path)
    {
        return None;
    }
    let mut segments = repository_parts
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    segments.push("blob".to_owned());
    segments.push(source.source_sha.clone());
    segments.extend(source.path.split('/').map(str::to_owned));
    g0_encoded_url_path(&segments)
}

fn g0_repo_contents_endpoint(repository: &str, path: &str) -> Option<String> {
    let repository_parts = repository.split('/').collect::<Vec<_>>();
    if repository_parts.len() != 2
        || repository_parts.iter().any(|part| part.is_empty())
        || !g0_relative_source_path_is_safe(path)
    {
        return None;
    }
    let mut segments = vec!["repos".to_owned()];
    segments.extend(repository_parts.into_iter().map(str::to_owned));
    segments.push("contents".to_owned());
    segments.extend(path.split('/').map(str::to_owned));
    g0_encoded_url_path(&segments)
}

fn g0_encoded_url_path(segments: &[String]) -> Option<String> {
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(segment.as_str(), "." | ".."))
    {
        return None;
    }
    let mut url = Url::parse(GITHUB_API_BASE).ok()?;
    {
        let mut path = url.path_segments_mut().ok()?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Some(url.path().to_owned())
}

fn g0_workflow_file_path(path: &str) -> bool {
    let Some(filename) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    !filename.is_empty()
        && !filename.contains('/')
        && !path.split('/').any(|component| component == "..")
        && (filename.ends_with(".yml") || filename.ends_with(".yaml"))
}

fn g0_relative_source_path_is_safe(path: &str) -> bool {
    let path = path.trim().trim_start_matches("./");
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|component| component == "..")
}

fn decode_g0_request_field(encoded: &str) -> Option<Vec<u8>> {
    if !g0_request_field_size_within_limit(encoded) {
        return None;
    }
    decode_g0_base64_limited(encoded, G0_MAX_REQUEST_FIELD_BYTES)
}

fn g0_request_field_size_within_limit(encoded: &str) -> bool {
    encoded.len() <= G0_MAX_REQUEST_FIELD_BASE64_BYTES
        && g0_base64_decoded_length(encoded)
            .is_some_and(|decoded_length| decoded_length <= G0_MAX_REQUEST_FIELD_BYTES)
}

fn g0_raw_object_bytes_within_limit(raw: &G0RawObjectRef) -> bool {
    if raw.canonicalization == "raw-utf8" {
        return g0_workflow_source_size_within_limit(&raw.bytes_base64, raw.byte_length);
    }
    raw.bytes_base64.len() <= G0_MAX_RAW_OBJECT_BASE64_BYTES
        && raw.byte_length <= G0_MAX_RAW_OBJECT_BYTES as u64
        && g0_base64_decoded_length(&raw.bytes_base64).is_some_and(|decoded_length| {
            decoded_length <= G0_MAX_RAW_OBJECT_BYTES && decoded_length as u64 == raw.byte_length
        })
}

fn decode_g0_raw_object_bytes(raw: &G0RawObjectRef) -> Option<Vec<u8>> {
    if !g0_raw_object_bytes_within_limit(raw) {
        return None;
    }
    let decoded = if raw.canonicalization == "raw-utf8" {
        decode_g0_workflow_source_bytes(&raw.bytes_base64, raw.byte_length)?
    } else {
        decode_g0_base64_limited(&raw.bytes_base64, G0_MAX_RAW_OBJECT_BYTES)?
    };
    (decoded.len() as u64 == raw.byte_length).then_some(decoded)
}

fn decode_g0_workflow_source_bytes(encoded: &str, byte_length: u64) -> Option<Vec<u8>> {
    if !g0_workflow_source_size_within_limit(encoded, byte_length) {
        return None;
    }
    let bytes = decode_g0_base64_limited(encoded, G0_MAX_WORKFLOW_SOURCE_BYTES)?;
    (bytes.len() as u64 == byte_length).then_some(bytes)
}

fn decode_g0_base64_limited(encoded: &str, max_decoded_bytes: usize) -> Option<Vec<u8>> {
    if encoded.len() > max_decoded_bytes.div_ceil(3) * 4 {
        return None;
    }
    let decoded_length = g0_base64_decoded_length(encoded)?;
    if decoded_length > max_decoded_bytes {
        return None;
    }
    let decoded = BASE64.decode(encoded).ok()?;
    (decoded.len() == decoded_length).then_some(decoded)
}

fn g0_base64_decoded_length(encoded: &str) -> Option<usize> {
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    let content_length = bytes.len().checked_sub(padding)?;
    if bytes[..content_length].contains(&b'=') {
        return None;
    }
    bytes
        .len()
        .checked_div(4)?
        .checked_mul(3)?
        .checked_sub(padding)
}

fn g0_workflow_source_size_within_limit(encoded: &str, byte_length: u64) -> bool {
    encoded.len() <= G0_MAX_WORKFLOW_SOURCE_BASE64_BYTES
        && byte_length <= G0_MAX_WORKFLOW_SOURCE_BYTES as u64
        && g0_base64_decoded_length(encoded).is_some_and(|decoded_length| {
            decoded_length <= G0_MAX_WORKFLOW_SOURCE_BYTES && decoded_length as u64 == byte_length
        })
}

fn g0_workflow_inventory_sources_size_within_limit(workflow: &G0WorkflowInventory) -> bool {
    g0_workflow_source_size_within_limit(&workflow.source.bytes_base64, workflow.source.byte_length)
        && workflow
            .reusable_workflows
            .iter()
            .chain(workflow.actions.iter())
            .chain(workflow.scanners.iter())
            .all(|dependency| {
                g0_workflow_source_size_within_limit(
                    &dependency.source.bytes_base64,
                    dependency.source.byte_length,
                )
            })
}

fn g0_api_base(value: &str) -> bool {
    matches!(value, GITHUB_API_BASE | "https://api.github.com/")
}

fn check_g0_dependency_source(
    field: &str,
    source: &G0WorkflowSource,
    raw_ids: &BTreeSet<String>,
    raw_objects: &[G0RawObjectRef],
    requests: &[G0RequestRecord],
    findings: &mut Vec<Finding>,
) {
    let decoded = decode_g0_workflow_source_bytes(&source.bytes_base64, source.byte_length);
    if source.repository.trim().is_empty()
        || !g0_relative_source_path_is_safe(&source.path)
        || !valid_sha(&source.revision)
        || !valid_sha(&source.source_sha)
        || source.revision != source.source_sha
        || !workflow_source_url_matches(source)
        || !matches!(source.media_type.as_str(), "text/yaml" | "application/yaml")
        || source.canonicalization != "raw-utf8"
        || !valid_digest(&source.sha256)
        || !g0_storage_ref(&source.storage_ref, &source.sha256)
        || decoded
            .as_ref()
            .is_none_or(|bytes| digest_bytes(bytes) != source.sha256)
        || source.raw_object_refs.is_empty()
        || source
            .raw_object_refs
            .iter()
            .any(|raw_id| !raw_ids.contains(raw_id))
        || !source_has_raw_binding(source, raw_objects, requests)
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
    raw_objects: &[G0RawObjectRef],
    requests: &[G0RequestRecord],
) -> bool {
    let Some(bytes) = decode_g0_workflow_source_bytes(&source.bytes_base64, source.byte_length)
    else {
        return false;
    };
    !source.raw_object_refs.is_empty()
        && source.raw_object_refs.iter().all(|raw_id| {
            raw_objects.iter().any(|raw| {
                raw.raw_id == *raw_id
                    && source_raw_response_matches(source, raw, requests)
                    && raw.sha256 == source.sha256
                    && decode_g0_workflow_source_bytes(&raw.bytes_base64, raw.byte_length)
                        .is_some_and(|raw_bytes| raw_bytes == bytes)
            })
        })
}

fn source_raw_response_matches(
    source: &G0WorkflowSource,
    raw: &G0RawObjectRef,
    requests: &[G0RequestRecord],
) -> bool {
    let Some(request) = requests
        .iter()
        .find(|request| request.request_id == raw.request_id)
    else {
        return false;
    };
    let Some(expected_endpoint) = g0_repo_contents_endpoint(&source.repository, &source.path)
    else {
        return false;
    };
    let query = decode_g0_request_field(&request.query_base64);
    request.api == G0ApiKind::Rest
        && request.method == "GET"
        && request.endpoint_or_operation == expected_endpoint
        && request
            .accept
            .eq_ignore_ascii_case("application/vnd.github.raw+json")
        && request.response_raw_ref == raw.raw_id
        && request.http_status == 200
        && request.complete
        && request.truncation_reason.is_none()
        && request.state == G0RequestState::Complete
        && request.page.number == 1
        && !request.page.has_next_page
        && request.page.link_next.is_none()
        && request.page.cursor_out.is_none()
        && query.as_ref().is_some_and(|query| {
            g0_rest_query_pairs(query)
                .is_some_and(|pairs| pairs == vec![("ref".to_owned(), source.source_sha.clone())])
        })
        && matches!(raw.media_type.as_str(), "text/yaml" | "application/yaml")
        && raw.canonicalization == "raw-utf8"
        && raw.original_sha256 == raw.sha256
        && raw.original_byte_length == raw.byte_length
}

fn g0_child_edge_parent_matches_source(
    edge: &crate::g0_workflow::DerivedChildEdge,
    source: &G0WorkflowSource,
) -> bool {
    edge.parent_repository == source.repository
        && edge.parent_workflow_path == source.path
        && source.revision == source.source_sha
        && edge.parent_source_sha == source.revision
}

fn g0_child_edge_relation_matches_event(edge: &crate::g0_workflow::DerivedChildEdge) -> bool {
    matches!(
        (edge.relation.as_str(), edge.event.as_str()),
        ("reusable_workflow", "workflow_call")
            | ("workflow_run", "workflow_run")
            | ("dispatch", "workflow_dispatch")
    )
}

// A workflow trigger starts a root run; it does not prove that this workflow
// invoked a child run. Only a job-level `uses:` declaration gives this plan a
// source-bound child relation. The Runner receives `github.event_name` and the
// webhook payload as per-run context, but does not derive invocation edges.
fn g0_child_edge_is_supported(edge: &crate::g0_workflow::DerivedChildEdge) -> bool {
    edge.relation == "reusable_workflow" && edge.event == "workflow_call"
}

fn g0_child_edge_matches_parent_source_plan(
    edge: &crate::g0_workflow::DerivedChildEdge,
    candidate: &crate::g0_workflow::DerivedChildEdge,
    parent_source: &G0WorkflowSource,
) -> bool {
    g0_child_edge_parent_matches_source(candidate, parent_source)
        && candidate.root_workload_id == candidate.workload_id
        && edge.workload_id == candidate.workload_id
        && edge.repository == candidate.repository
        && edge.workflow_path == candidate.workflow_path
        && edge.event == candidate.event
        && edge.relation == candidate.relation
        && edge.source_sha == candidate.source_sha
        && edge.parent_repository == candidate.parent_repository
        && edge.parent_workflow_path == candidate.parent_workflow_path
        && edge.parent_source_sha == candidate.parent_source_sha
}

fn g0_child_edge_workload_matches_parent_source(
    edge: &crate::g0_workflow::DerivedChildEdge,
    root_source: &G0WorkflowSource,
    root_plan: &DerivedWorkflowPlan,
    dependencies: &[G0WorkflowDependency],
    workflow_derivation: &mut WorkflowDerivationContext,
) -> bool {
    if g0_child_edge_parent_matches_source(edge, root_source) {
        return root_plan.child_edges.iter().any(|candidate| {
            g0_child_edge_matches_parent_source_plan(edge, candidate, root_source)
        });
    }

    let parents = dependencies
        .iter()
        .filter(|dependency| {
            dependency.kind == "reusable_workflow"
                && dependency.source.repository == edge.parent_repository
                && dependency.source.path == edge.parent_workflow_path
                && dependency.source.revision == edge.parent_source_sha
                && dependency.source.source_sha == edge.parent_source_sha
        })
        .collect::<Vec<_>>();
    let [parent] = parents.as_slice() else {
        return false;
    };
    let Ok(parent_plan) =
        derive_workflow_plan_with_context(&parent.source, dependencies, workflow_derivation)
    else {
        return false;
    };
    parent_plan
        .child_edges
        .iter()
        .any(|candidate| g0_child_edge_matches_parent_source_plan(edge, candidate, &parent.source))
}

fn g0_reusable_workflow_jobs_match_root_edges(
    root_source: &G0WorkflowSource,
    plan: &DerivedWorkflowPlan,
) -> bool {
    let reusable_jobs = plan
        .jobs
        .iter()
        .filter(|job| job.uses_reusable_workflow)
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let direct_edges = plan
        .child_edges
        .iter()
        .filter(|edge| {
            g0_child_edge_is_supported(edge)
                && g0_child_edge_parent_matches_source(edge, root_source)
        })
        .map(|edge| edge.workload_id.clone())
        .collect::<Vec<_>>();
    let direct_jobs = direct_edges.iter().cloned().collect::<BTreeSet<_>>();
    reusable_jobs == direct_jobs && direct_jobs.len() == direct_edges.len()
}

fn g0_child_edge_parent_descends_from_reviewed_child(
    edge: &crate::g0_workflow::DerivedChildEdge,
    root_workload_id: &str,
    root_source: &G0WorkflowSource,
    child: &ChildWorkflowSpec,
    plan: &DerivedWorkflowPlan,
    reusable_workflows: &[G0WorkflowDependency],
) -> bool {
    if child.event != "workflow_call" {
        return false;
    }
    let mut reachable_sources = BTreeSet::new();
    for candidate in plan
        .child_edges
        .iter()
        .filter(|candidate| g0_child_edge_is_supported(candidate))
    {
        if candidate.root_workload_id != root_workload_id
            || !g0_child_edge_parent_matches_source(candidate, root_source)
            || candidate.repository != child.repository
            || candidate.workflow_path != child.workflow_path
            || candidate.event != child.event
        {
            continue;
        }
        let direct_child_source_count = reusable_workflows
            .iter()
            .filter(|dependency| {
                dependency.kind == "reusable_workflow"
                    && dependency.source.repository == child.repository
                    && dependency.source.path == child.workflow_path
                    && dependency.source.revision == candidate.source_sha
                    && dependency.source.source_sha == candidate.source_sha
            })
            .count();
        if direct_child_source_count == 1 {
            reachable_sources.insert((
                candidate.repository.clone(),
                candidate.workflow_path.clone(),
                candidate.source_sha.clone(),
            ));
        }
    }
    if reachable_sources.is_empty() {
        return false;
    }
    loop {
        let mut added_source = false;
        for candidate in plan.child_edges.iter().filter(|candidate| {
            g0_child_edge_is_supported(candidate) && candidate.root_workload_id == root_workload_id
        }) {
            let parent_source = (
                candidate.parent_repository.clone(),
                candidate.parent_workflow_path.clone(),
                candidate.parent_source_sha.clone(),
            );
            if !reachable_sources.contains(&parent_source) {
                continue;
            }
            let parent_source_count = reusable_workflows
                .iter()
                .filter(|dependency| {
                    dependency.kind == "reusable_workflow"
                        && g0_child_edge_parent_matches_source(candidate, &dependency.source)
                })
                .count();
            if parent_source_count == 1 {
                added_source |= reachable_sources.insert((
                    candidate.repository.clone(),
                    candidate.workflow_path.clone(),
                    candidate.source_sha.clone(),
                ));
            }
        }
        if !added_source {
            break;
        }
    }
    reachable_sources.contains(&(
        edge.parent_repository.clone(),
        edge.parent_workflow_path.clone(),
        edge.parent_source_sha.clone(),
    ))
}

fn g0_child_edge_parent_matches_observation(
    child: &ChildWorkflowObservation,
    edge: &crate::g0_workflow::DerivedChildEdge,
    repository: &str,
    execution: &ExecutionObservation,
    root_source: &G0WorkflowSource,
) -> bool {
    if child.parent_repository != edge.parent_repository
        || child.parent_workflow_path != edge.parent_workflow_path
        || child.parent_source_sha != edge.parent_source_sha
    {
        return false;
    }
    if g0_child_edge_parent_matches_source(edge, root_source)
        && edge.parent_repository == repository
        && edge.parent_workflow_path == execution.workflow_path
        && edge.parent_source_sha == execution.workflow_revision
        && root_source.revision == execution.workflow_revision
    {
        return child.parent_run_id == execution.run_id
            && child.parent_run_attempt == execution.run_attempt;
    }
    execution
        .child_workflows
        .iter()
        .filter(|parent| {
            parent.run_id == child.parent_run_id
                && parent.run_attempt == child.parent_run_attempt
                && parent.repository == edge.parent_repository
                && parent.workflow_path == edge.parent_workflow_path
                && parent.source_sha == edge.parent_source_sha
        })
        .count()
        == 1
}

fn g0_manifest_child_edge_matches(
    manifest: &ManifestRepository,
    provider: &str,
    edge: &crate::g0_workflow::DerivedChildEdge,
    repository: &str,
    root_source: &G0WorkflowSource,
    plan: &DerivedWorkflowPlan,
    reusable_workflows: &[G0WorkflowDependency],
) -> bool {
    if !g0_child_edge_is_supported(edge) {
        return false;
    }
    manifest
        .expected_jobs
        .iter()
        .filter(|job| {
            job.job_id == edge.root_workload_id
                && job.provider == provider
                && job.required
                && job.child_workflow.as_ref().is_some_and(|target| {
                    if g0_child_edge_parent_matches_source(edge, root_source)
                        && edge.parent_repository == repository
                    {
                        target.repository == edge.repository
                            && target.workflow_path == edge.workflow_path
                            && target.event == edge.event
                    } else {
                        g0_child_edge_parent_descends_from_reviewed_child(
                            edge,
                            &edge.root_workload_id,
                            root_source,
                            target,
                            plan,
                            reusable_workflows,
                        )
                    }
                })
        })
        .count()
        == 1
}

struct G0ChildEdgeMatchContext<'a> {
    repository: &'a str,
    provider: &'a str,
    execution: &'a ExecutionObservation,
    root_source: &'a G0WorkflowSource,
    manifest: &'a ManifestRepository,
    plan: &'a DerivedWorkflowPlan,
    reusable_workflows: &'a [G0WorkflowDependency],
}

fn g0_child_edge_matches_observation(
    child: &ChildWorkflowObservation,
    edge: &crate::g0_workflow::DerivedChildEdge,
    context: &G0ChildEdgeMatchContext<'_>,
) -> bool {
    g0_child_edge_is_supported(edge)
        && edge.repository == child.repository
        && edge.workflow_path == child.workflow_path
        && edge.event == child.event
        && edge.source_sha == child.source_sha
        && g0_child_edge_parent_matches_observation(
            child,
            edge,
            context.repository,
            context.execution,
            context.root_source,
        )
        && g0_manifest_child_edge_matches(
            context.manifest,
            context.provider,
            edge,
            context.repository,
            context.root_source,
            context.plan,
            context.reusable_workflows,
        )
}

fn check_g0_derived_plan(
    repository: &str,
    manifest: &ManifestRepository,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    findings: &mut Vec<Finding>,
) {
    let mut workflow_derivation = WorkflowDerivationContext::default();
    check_g0_derived_plan_with_context(
        repository,
        manifest,
        workflow,
        plan,
        &mut workflow_derivation,
        findings,
    );
}

fn check_g0_derived_plan_with_context(
    repository: &str,
    manifest: &ManifestRepository,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    workflow_derivation: &mut WorkflowDerivationContext,
    findings: &mut Vec<Finding>,
) {
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
    if !g0_reusable_workflow_jobs_match_root_edges(&workflow.source, plan) {
        finding(
            findings,
            "g0-workflow-child",
            repository,
            "workflow.source/child_edges",
            "source-derived reusable workflow jobs must have exactly one matching direct workflow_call edge",
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
    for edge in &plan.child_edges {
        if !g0_child_edge_relation_matches_event(edge) {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "workflow.source/child_edges",
                format!(
                    "source-derived child relation {} does not match event {}",
                    edge.relation, edge.event
                ),
            );
        }
    }
    let derived_children = plan
        .child_edges
        .iter()
        .filter(|edge| g0_child_edge_is_supported(edge))
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
    let dependencies = workflow
        .reusable_workflows
        .iter()
        .chain(workflow.actions.iter())
        .chain(workflow.scanners.iter())
        .cloned()
        .collect::<Vec<_>>();
    for edge in plan
        .child_edges
        .iter()
        .filter(|edge| g0_child_edge_is_supported(edge))
    {
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
        if !g0_child_edge_workload_matches_parent_source(
            edge,
            &workflow.source,
            plan,
            &dependencies,
            workflow_derivation,
        ) {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "workflow.source/child_edges.workload_id",
                format!(
                    "source-derived child workload {} is absent from its exact parent workflow source",
                    edge.workload_id
                ),
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
        let parent_is_root_source = g0_child_edge_parent_matches_source(edge, &workflow.source)
            && edge.parent_repository == repository;
        let matches = if parent_is_root_source {
            expected.child_workflow.as_ref().is_some_and(|child| {
                child.repository == edge.repository
                    && child.workflow_path == edge.workflow_path
                    && child.event == edge.event
            })
        } else {
            expected.child_workflow.as_ref().is_some_and(|child| {
                g0_child_edge_parent_descends_from_reviewed_child(
                    edge,
                    &edge.root_workload_id,
                    &workflow.source,
                    child,
                    plan,
                    &workflow.reusable_workflows,
                )
            })
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

fn check_g0_workflow_events(
    repository: &str,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    findings: &mut Vec<Finding>,
) {
    let captured_events = workflow.events.iter().cloned().collect::<BTreeSet<_>>();
    if plan.events != captured_events || captured_events.len() != workflow.events.len() {
        finding(
            findings,
            "g0-workflow-events",
            repository,
            "workflows.events",
            "workflow trigger inventory must equal events parsed from immutable source bytes",
        );
    }
}

fn check_g0_workflow_projection(
    repository: &str,
    manifest: Option<&ManifestRepository>,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    raw_ids: &BTreeSet<String>,
    workflow_derivation: &mut WorkflowDerivationContext,
    findings: &mut Vec<Finding>,
) {
    check_g0_workflow_events(repository, workflow, plan, findings);
    check_g0_source_jobs(repository, manifest, workflow, plan, raw_ids, findings);
    if let Some(manifest) = manifest {
        check_g0_derived_plan_with_context(
            repository,
            manifest,
            workflow,
            plan,
            workflow_derivation,
            findings,
        );
    }
}

fn check_g0_root_execution_event(
    repository: &str,
    source: &G0WorkflowSource,
    plan: &DerivedWorkflowPlan,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    if execution.workflow_path == source.path
        && execution.workflow_revision == source.revision
        && !plan.events.contains(execution.event.as_str())
    {
        finding(
            findings,
            "execution-event-source",
            repository,
            "execution.event",
            "observed root execution event is absent from the triggers parsed from its immutable workflow source",
        );
    }
}

fn check_g0_execution_workflow_source(
    repository: &str,
    execution: &ExecutionObservation,
    captured_sources: &[(&G0WorkflowInventory, Option<DerivedWorkflowPlan>)],
    findings: &mut Vec<Finding>,
) {
    let matching_sources = captured_sources
        .iter()
        .filter(|(workflow, _)| {
            workflow.source.path == execution.workflow_path
                && workflow.source.revision == execution.workflow_revision
        })
        .collect::<Vec<_>>();
    let [(workflow, Some(plan))] = matching_sources.as_slice() else {
        finding(
            findings,
            "g0-execution-workflow-source",
            repository,
            "snapshot.executions.workflow_path/workflow_revision",
            "every execution must match exactly one captured, derivable immutable workflow source before its event is compared",
        );
        return;
    };
    check_g0_root_execution_event(repository, &workflow.source, plan, execution, findings);
}

fn check_g0_repositories(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    workflow_derivation: &mut WorkflowDerivationContext,
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
                manifest
                    .repositories
                    .iter()
                    .find(|candidate| candidate.repository == repo.repository)
                    .map(|candidate| {
                        (
                            candidate.workflow_path.as_str(),
                            candidate.workflow_revision.as_str(),
                        )
                    }),
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
        let mut required_policy_contexts = BTreeMap::<&str, &str>::new();
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
                    || required_policy_contexts
                        .insert(&required.context, &required.app_id)
                        .is_some()
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
        let mut source_plans = Vec::with_capacity(repo.workflows.len());
        let snapshot_repository = snapshot
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository);
        for workflow in &repo.workflows {
            let source_plan = if g0_workflow_inventory_sources_size_within_limit(workflow) {
                let dependencies = workflow
                    .reusable_workflows
                    .iter()
                    .chain(workflow.actions.iter())
                    .chain(workflow.scanners.iter())
                    .cloned()
                    .collect::<Vec<_>>();
                check_g0_workflow_source_with_context(
                    "evidence.g0_inventory.collector_snapshot.workflow.source",
                    &workflow.source,
                    G0WorkflowSourceContext {
                        repository: &repo.repository,
                        default_branch_sha: &repo.default_branch_sha,
                        dependencies: &dependencies,
                        raw_objects: &collector.raw_objects,
                        requests: &collector.requests,
                    },
                    workflow_derivation,
                    findings,
                )
            } else {
                finding(
                    findings,
                    "g0-workflow-source",
                    &repo.repository,
                    "evidence.g0_inventory.collector_snapshot.workflow.source",
                    "workflow source and dependencies must stay within the one MiB checker input limit",
                );
                None
            };
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
            check_g0_generated_state_reference(
                &repo.repository,
                workflow,
                repo,
                snapshot_repository,
                &collector.requests,
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
                    &collector.raw_objects,
                    &collector.requests,
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
            let reviewed_manifest = manifest
                .repositories
                .iter()
                .find(|candidate| candidate.repository == repo.repository)
                .filter(|candidate| {
                    workflow.source.path == candidate.workflow_path
                        && workflow.source.revision == candidate.workflow_revision
                });
            if let Some(plan) = source_plan.as_ref() {
                check_g0_workflow_projection(
                    &repo.repository,
                    reviewed_manifest,
                    workflow,
                    plan,
                    raw_ids,
                    workflow_derivation,
                    findings,
                );
            }
            source_plans.push((workflow, source_plan));
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
        if let Some(snapshot_repository) = snapshot_repository {
            for execution in &snapshot_repository.main_executions {
                check_g0_execution_workflow_source(
                    &repo.repository,
                    execution,
                    &source_plans,
                    findings,
                );
            }
            for execution in snapshot_repository
                .open_prs
                .iter()
                .flat_map(|pull_request| pull_request.executions.iter())
            {
                check_g0_execution_workflow_source(
                    &repo.repository,
                    execution,
                    &source_plans,
                    findings,
                );
            }
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
                raw_ids,
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
    let mut known_run_sources = BTreeMap::<u64, BTreeSet<String>>::new();
    for check in context.main_checks {
        known_run_sources
            .entry(check.workflow_run_id)
            .or_default()
            .insert(check.source_sha.clone());
    }
    let mut known_source_shas = BTreeSet::from([context.default_branch_sha.to_owned()]);
    for pr in context.open_prs {
        known_source_shas.insert(pr.head_sha.clone());
        known_source_shas.insert(pr.base_sha.clone());
        if let Some(tested_merge_sha) = &pr.tested_merge_sha {
            known_source_shas.insert(tested_merge_sha.clone());
        }
        if let Some(merge_group_sha) = &pr.merge_group_sha {
            known_source_shas.insert(merge_group_sha.clone());
        }
        for check in &pr.required_check_producers {
            known_run_sources
                .entry(check.workflow_run_id)
                .or_default()
                .insert(check.source_sha.clone());
        }
    }

    for artifact in artifacts {
        if artifact.artifact_id == 0
            || !artifact_ids.insert(artifact.artifact_id)
            || artifact.name.trim().is_empty()
            || artifact.run_id == 0
            || !valid_sha(&artifact.run_head_sha)
            || !known_source_shas.contains(&artifact.run_head_sha)
            || !known_run_sources
                .get(&artifact.run_id)
                .is_some_and(|sources| sources.contains(&artifact.run_head_sha))
            || !valid_digest(&artifact.digest)
            || artifact.expired
            || !g0_artifact_source_url(repository, artifact.artifact_id, &artifact.source_url)
        {
            finding(
                findings,
                "g0-artifact-identity",
                repository,
                "repositories.artifacts",
                "artifact rows require unique IDs, non-empty names, source-bound run identity, unexpired digest, and canonical API URL",
            );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.artifact.raw_object_refs",
            &artifact.raw_object_refs,
            context.raw_ids,
            findings,
        );
        let Some(raw) = artifact
            .raw_object_refs
            .iter()
            .find_map(|raw_id| context.raw_objects.iter().find(|raw| raw.raw_id == *raw_id))
        else {
            finding(
                findings,
                "g0-artifact-list-response",
                repository,
                "repositories.artifacts",
                "artifact metadata must reference its captured artifact-list response",
            );
            continue;
        };
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
            || request.response_raw_ref != raw.raw_id
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
                "artifact metadata raw object must be the successful response for its exact run's artifact-list request",
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
    manifest: Option<&ManifestRepository>,
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
    let expected_jobs = manifest.map(|manifest| {
        manifest
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
            .collect::<BTreeSet<_>>()
    });
    let mut assignments_by_job = BTreeMap::<String, BTreeSet<BTreeMap<String, String>>>::new();
    let mut duplicate_assignments = false;
    for job in &plan.jobs {
        duplicate_assignments |= !assignments_by_job
            .entry(job.job_id.clone())
            .or_default()
            .insert(job.matrix.clone());
    }
    if duplicate_assignments {
        finding(
            findings,
            "g0-source-job-matrix",
            repository,
            "repositories.workflows.source_jobs",
            "each source-derived logical job must have unique concrete matrix assignments",
        );
    }
    if assignments_by_job
        .values()
        .any(|assignments| assignments.len() > 1)
    {
        finding(
            findings,
            "g0-source-job-matrix",
            repository,
            "repositories.workflows.source_jobs",
            "finite matrix expands a logical job into multiple instances, but the reviewed source-job contract has no concrete matrix identity; collector must publish every instance before this gate can pass",
        );
    }
    if expected_jobs
        .as_ref()
        .is_some_and(|expected_jobs| source_jobs != *expected_jobs)
    {
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
                    .and_then(|manifest| {
                        manifest
                            .expected_jobs
                            .iter()
                            .find(|expected| expected.job_id == job.job_id)
                    })
                    .map_or_else(
                        || job.job_id.clone(),
                        |expected| expected.workload_id.clone(),
                    ),
                job.provider.clone(),
                job.platform.clone(),
                job.architecture.clone(),
                manifest
                    .and_then(|manifest| {
                        manifest
                            .expected_jobs
                            .iter()
                            .find(|expected| expected.job_id == job.job_id)
                    })
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

struct G0CheckExpectation<'a> {
    contexts: &'a BTreeSet<(String, String)>,
    source_sha: &'a str,
    checkout_sha: &'a str,
    event: &'a str,
    raw_ids: &'a BTreeSet<String>,
}

fn check_g0_check_producers(
    repository: &str,
    checks: &[G0CheckProducer],
    expectation: G0CheckExpectation<'_>,
    findings: &mut Vec<Finding>,
) {
    let mut seen = BTreeSet::new();
    let mut seen_contexts = BTreeMap::<&str, &str>::new();
    let mut check_run_ids = BTreeSet::new();
    let mut producer_jobs = BTreeSet::new();
    for check in checks {
        let key = (check.context.clone(), check.app_id.clone());
        if !seen.insert(key.clone())
            || seen_contexts
                .insert(&check.context, &check.app_id)
                .is_some()
            || !check_run_ids.insert(check.check_run_id)
            || !producer_jobs.insert((check.workflow_run_id, check.run_attempt, check.job_id))
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
            || check.app_slug.trim().is_empty()
            || check.check_suite_id == 0
            || check.check_run_id == 0
            || check.workflow_run_id == 0
            || check.run_attempt == 0
            || check.job_id == 0
            || check.job_run_id != check.workflow_run_id
            || check.job_run_attempt != check.run_attempt
            || check.job_check_run_id != check.check_run_id
            || !valid_sha(&check.job_source_sha)
            || check.job_source_sha != check.source_sha
            || !valid_sha(&check.source_sha)
            || check.source_sha != expectation.source_sha
            || !valid_sha(&check.actual_checkout_sha)
            || check.actual_checkout_sha != expectation.checkout_sha
            || check.event != expectation.event
            || check.status != "completed"
            || check.conclusion != "success"
            || !job_url_matches_repository(
                &check.job_html_url,
                repository,
                check.workflow_run_id,
                check.job_id,
            )
            || !check_run_url_matches_repository(&check.html_url, repository, check.check_run_id)
            || match check.provider {
                G0CheckProvider::GithubActions => check.app_slug != "github-actions",
                G0CheckProvider::ExternalApp => check.app_slug == "github-actions",
            }
        {
            finding(
                findings,
                "g0-check-producer",
                repository,
                "check_producers",
            "check producer must bind captured App/provider, check/run/job identities, current source SHA, event, successful status/conclusion, and provider html_url",
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct G0CheckJoinRow {
    run_id: u64,
    run_attempt: u32,
    check_run_id: u64,
    workflow_path: String,
    workflow_revision: String,
    event: String,
    trigger_source_sha: String,
    actual_checkout_sha: String,
    context: String,
    app_id: String,
    job_id: String,
    status: String,
    conclusion: String,
    source_url: String,
}

fn g0_observed_check_join_row(
    execution: &ExecutionObservation,
    check: &CheckObservation,
) -> G0CheckJoinRow {
    G0CheckJoinRow {
        run_id: execution.run_id,
        run_attempt: execution.run_attempt,
        check_run_id: check.run_id,
        workflow_path: execution.workflow_path.clone(),
        workflow_revision: execution.workflow_revision.clone(),
        event: execution.event.clone(),
        trigger_source_sha: execution.trigger_source_sha.clone(),
        actual_checkout_sha: execution.actual_checkout_sha.clone(),
        context: check.context.clone(),
        app_id: check.app_id.clone(),
        job_id: check.job_id.clone(),
        status: check.status.clone(),
        conclusion: check.conclusion.clone(),
        source_url: check.source_url.clone(),
    }
}

fn g0_producer_check_join_row(
    check: &G0CheckProducer,
    workflow_path: &str,
    workflow_revision: &str,
) -> G0CheckJoinRow {
    G0CheckJoinRow {
        run_id: check.workflow_run_id,
        run_attempt: check.run_attempt,
        check_run_id: check.workflow_run_id,
        workflow_path: workflow_path.to_owned(),
        workflow_revision: workflow_revision.to_owned(),
        event: check.event.clone(),
        trigger_source_sha: check.source_sha.clone(),
        actual_checkout_sha: check.actual_checkout_sha.clone(),
        context: check.context.clone(),
        app_id: check.app_id.clone(),
        job_id: check.job_id.to_string(),
        status: check.status.clone(),
        conclusion: check.conclusion.clone(),
        source_url: check.html_url.clone(),
    }
}

fn check_g0_main_checks_against_snapshot(
    repository: &str,
    checks: &[G0CheckProducer],
    snapshot: &SnapshotRepository,
    reviewed_workflow: Option<(&str, &str)>,
    findings: &mut Vec<Finding>,
) {
    if checks.is_empty() && snapshot.main_executions.is_empty() {
        // G0 is structural inventory today. Keep its existing missing-check
        // finding, but do not invent execution evidence for this stage.
        return;
    }

    let mut snapshot_runs = Vec::new();
    let mut snapshot_rows = Vec::new();
    let mut seen_runs = BTreeSet::new();
    for execution in &snapshot.main_executions {
        if !seen_runs.insert((execution.run_id, execution.run_attempt)) {
            finding(
                findings,
                "g0-main-execution-duplicate",
                repository,
                "repositories.main_executions",
                "main execution run ID/attempt identities must be unique",
            );
        }
        snapshot_runs.push((execution.run_id, execution.run_attempt));
        if reviewed_workflow.is_none_or(|(path, revision)| {
            execution.workflow_path != path || execution.workflow_revision != revision
        }) {
            finding(
                findings,
                "g0-check-snapshot-mismatch",
                repository,
                "repositories.main_executions.workflow_path/workflow_revision",
                "main execution must bind the exact reviewed workflow path and revision",
            );
        }
        snapshot_rows.extend(
            execution
                .required_checks
                .iter()
                .map(|check| g0_observed_check_join_row(execution, check)),
        );
    }

    let mut producer_runs = checks
        .iter()
        .map(|check| (check.workflow_run_id, check.run_attempt))
        .collect::<Vec<_>>();
    producer_runs.sort_unstable();
    producer_runs.dedup();
    snapshot_runs.sort_unstable();
    snapshot_runs.dedup();

    for check in checks {
        let matches = snapshot
            .main_executions
            .iter()
            .filter(|execution| {
                execution.run_id == check.workflow_run_id
                    && execution.run_attempt == check.run_attempt
            })
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0]
                .jobs
                .iter()
                .filter(|job| job.job_id == check.job_id.to_string())
                .count()
                != 1
        {
            finding(
                findings,
                "g0-check-snapshot-mismatch",
                repository,
                "repositories.main_checks.job_id",
                "main check must join one execution and exactly one job in that execution",
            );
        }
    }

    let mut producer_rows = checks
        .iter()
        .map(|check| {
            let (path, revision) = reviewed_workflow.unwrap_or_default();
            g0_producer_check_join_row(check, path, revision)
        })
        .collect::<Vec<_>>();
    producer_rows.sort_unstable();
    snapshot_rows.sort_unstable();
    if producer_runs != snapshot_runs || producer_rows != snapshot_rows {
        finding(
            findings,
            "g0-check-snapshot-mismatch",
            repository,
            "repositories.main_checks",
            "main check producers and snapshot execution/check rows must match exactly, including extras, workflow binding, job, status, and source",
        );
    }
}

fn check_g0_pull_request(
    repository: &str,
    pr: &G0PullRequestInventory,
    expected_contexts: &BTreeSet<(String, String)>,
    snapshot: &SnapshotDocument,
    raw_ids: &BTreeSet<String>,
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
        || pr
            .tested_merge_sha
            .as_deref()
            .is_some_and(|sha| !valid_sha(sha))
        || !g0_repo_source_url(repository, &pr.source_url)
    {
        finding(
            findings,
            "g0-pr-identity",
            repository,
            "open_prs",
            "PR inventory requires open identity, author/trust fields, head/base SHAs, and applicability; an available merge candidate must be valid",
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
    let mut binding_runs = BTreeSet::new();
    if pr.workflow_bindings.is_empty() {
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
            || binding.runs.is_empty()
        {
            finding(
                findings,
                "g0-pr-workflow-binding",
                repository,
                "open_prs.workflow_bindings",
                "workflow binding must identify immutable workflow/source/event and exact run attempts",
            );
        }
        for run in &binding.runs {
            if run.run_id == 0
                || run.run_attempt == 0
                || !binding_runs.insert((run.run_id, run.run_attempt))
            {
                finding(
                    findings,
                    "g0-pr-workflow-binding",
                    repository,
                    "open_prs.workflow_bindings.runs",
                    "workflow run ID/attempt pairs must be positive and unique per PR",
                );
            }
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.workflow_binding.raw_object_refs",
            &binding.raw_object_refs,
            raw_ids,
            findings,
        );
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
    let mut seen_contexts = BTreeMap::<&str, &str>::new();
    let mut check_run_ids = BTreeSet::new();
    let mut producer_jobs = BTreeSet::new();
    for check in &pr.required_check_producers {
        let key = (check.context.clone(), check.app_id.clone());
        if !seen_checks.insert(key.clone())
            || seen_contexts
                .insert(&check.context, &check.app_id)
                .is_some()
            || !check_run_ids.insert(check.check_run_id)
            || !producer_jobs.insert((check.workflow_run_id, check.run_attempt, check.job_id))
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
        let matching_binding_count = pr
            .workflow_bindings
            .iter()
            .filter(|binding| {
                binding
                    .runs
                    .iter()
                    .filter(|run| {
                        run.run_id == check.workflow_run_id && run.run_attempt == check.run_attempt
                    })
                    .count()
                    == 1
                    && binding.event == check.event
                    && binding.source_sha == check.source_sha
                    && binding.actual_checkout_sha == check.actual_checkout_sha
            })
            .count();
        if check.check_suite_id == 0
            || check.check_run_id == 0
            || check.workflow_run_id == 0
            || check.run_attempt == 0
            || check.job_id == 0
            || check.job_run_id != check.workflow_run_id
            || check.job_run_attempt != check.run_attempt
            || check.job_check_run_id != check.check_run_id
            || !valid_sha(&check.job_source_sha)
            || check.job_source_sha != check.source_sha
            || check.app_slug.trim().is_empty()
            || !g0_valid_app_id(&check.app_id)
            || !valid_sha(&check.source_sha)
            || check.source_sha != pr.head_sha
            || !valid_sha(&check.actual_checkout_sha)
            || check.event != "pull_request"
            || check.status != "completed"
            || check.conclusion != "success"
            || !job_url_matches_repository(
                &check.job_html_url,
                repository,
                check.workflow_run_id,
                check.job_id,
            )
            || !check_run_url_matches_repository(&check.html_url, repository, check.check_run_id)
            || match check.provider {
                G0CheckProvider::GithubActions => check.app_slug != "github-actions",
                G0CheckProvider::ExternalApp => check.app_slug == "github-actions",
            }
            || matching_binding_count != 1
        {
            finding(
                findings,
                "g0-pr-check-producer",
                repository,
                "open_prs.required_check_producers",
            "PR check producer must bind successful API status, source/event, workflow run/job, provider html_url, and App identity",
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
            let snapshot_run_rows = snapshot_pr
                .executions
                .iter()
                .map(|execution| (execution.run_id, execution.run_attempt))
                .collect::<Vec<_>>();
            let snapshot_runs = snapshot_run_rows.iter().copied().collect::<BTreeSet<_>>();
            if snapshot_runs.len() != snapshot_run_rows.len() {
                finding(
                    findings,
                    "g0-pr-execution-duplicate",
                    repository,
                    "open_prs.executions",
                    "PR snapshot execution run ID/attempt identities must be unique",
                );
            }
            if snapshot_runs != binding_runs {
                finding(
                    findings,
                    "g0-pr-workflow-binding",
                    repository,
                    "open_prs.workflow_bindings.runs",
                    "PR workflow bindings must cover the complete independently observed run-attempt set",
                );
            }
            for binding in &pr.workflow_bindings {
                for run in &binding.runs {
                    let matches = snapshot_pr
                        .executions
                        .iter()
                        .filter(|execution| {
                            execution.run_id == run.run_id
                                && execution.run_attempt == run.run_attempt
                        })
                        .collect::<Vec<_>>();
                    if matches.len() != 1
                        || matches[0].workflow_path != binding.workflow_path
                        || matches[0].workflow_revision != binding.workflow_revision
                        || matches[0].event != binding.event
                        || matches[0].trigger_source_sha != binding.source_sha
                        || matches[0].actual_checkout_sha != binding.actual_checkout_sha
                    {
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
                || pr.tested_merge_sha != snapshot_pr.merge_sha
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
            let mut producer_rows = Vec::new();
            for check in &pr.required_check_producers {
                let matching_bindings = pr
                    .workflow_bindings
                    .iter()
                    .filter(|binding| {
                        binding.runs.iter().any(|run| {
                            run.run_id == check.workflow_run_id
                                && run.run_attempt == check.run_attempt
                        })
                    })
                    .collect::<Vec<_>>();
                let matching_executions = snapshot_pr
                    .executions
                    .iter()
                    .filter(|execution| {
                        execution.run_id == check.workflow_run_id
                            && execution.run_attempt == check.run_attempt
                    })
                    .collect::<Vec<_>>();
                if matching_bindings.len() != 1 || matching_executions.len() != 1 {
                    finding(
                        findings,
                        "g0-pr-check-snapshot-mismatch",
                        repository,
                        "open_prs.required_check_producers",
                        "PR check producer does not match an independently observed run/check/job tuple",
                    );
                    continue;
                }
                let binding = matching_bindings[0];
                let execution = matching_executions[0];
                let job_id = check.job_id.to_string();
                if execution.workflow_path != binding.workflow_path
                    || execution.workflow_revision != binding.workflow_revision
                    || execution.event != binding.event
                    || execution.trigger_source_sha != binding.source_sha
                    || execution.actual_checkout_sha != binding.actual_checkout_sha
                    || binding.event != check.event
                    || binding.source_sha != check.source_sha
                    || binding.actual_checkout_sha != check.actual_checkout_sha
                    || execution
                        .jobs
                        .iter()
                        .filter(|job| job.job_id == job_id)
                        .count()
                        != 1
                {
                    finding(
                        findings,
                        "g0-pr-check-snapshot-mismatch",
                        repository,
                        "open_prs.required_check_producers.workflow/job",
                        "PR check must bind one exact workflow execution and exactly one matching job",
                    );
                }
                producer_rows.push(g0_producer_check_join_row(
                    check,
                    &binding.workflow_path,
                    &binding.workflow_revision,
                ));
            }
            let mut snapshot_rows = snapshot_pr
                .executions
                .iter()
                .flat_map(|execution| {
                    execution
                        .required_checks
                        .iter()
                        .map(|check| g0_observed_check_join_row(execution, check))
                })
                .collect::<Vec<_>>();
            producer_rows.sort_unstable();
            snapshot_rows.sort_unstable();
            if producer_rows != snapshot_rows {
                finding(
                    findings,
                    "g0-pr-check-snapshot-mismatch",
                    repository,
                    "open_prs.required_check_producers",
                    "PR check producers and snapshot execution/check rows must match exactly, including extras, workflow binding, job, status, and source",
                );
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
    let mut workflow_derivation = WorkflowDerivationContext::default();
    g0_source_child_workloads_with_context(manifest, collector, &mut workflow_derivation)
}

fn g0_source_child_workloads_with_context(
    manifest: &ManifestDocument,
    collector: &G0CollectorSnapshot,
    workflow_derivation: &mut WorkflowDerivationContext,
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
                || !g0_workflow_inventory_sources_size_within_limit(workflow)
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
            if let Ok(plan) = derive_workflow_plan_with_context(
                &workflow.source,
                &dependencies,
                workflow_derivation,
            ) {
                for edge in plan
                    .child_edges
                    .into_iter()
                    .filter(g0_child_edge_is_supported)
                {
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
                || pr
                    .tested_merge_sha
                    .as_deref()
                    .is_some_and(|sha| !valid_sha(sha))
            {
                finding(
                    findings,
                    "g0-reconciliation",
                    &revision.repository,
                    field,
                    "reconciled PR rows require positive number and immutable head/base SHAs; an available merge candidate must be valid",
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
    workflow_derivation: &mut WorkflowDerivationContext,
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
    let mut node_ids = BTreeSet::new();
    let mut adjacency = BTreeMap::<String, Vec<String>>::new();
    let mut node_edges = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
    let derived_child_workloads =
        g0_source_child_workloads_with_context(manifest, collector, workflow_derivation);
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
    }
}

fn check_g0_access(collector: &G0CollectorSnapshot, findings: &mut Vec<Finding>) {
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
    let authorized_scopes = collector
        .auth
        .safe_scopes
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let raw_ids = collector
        .raw_objects
        .iter()
        .map(|raw| raw.raw_id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for access in &collector.access {
        if !seen.insert(access.repository.clone())
            || !expected.contains(access.repository.as_str())
            || access.state != "complete"
            || !g0_scope_list_is_allowed(&access.scopes)
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
        let declared_scopes = access
            .scopes
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let request_scopes = g0_access_request_scopes(access, collector);
        if access
            .scopes
            .iter()
            .any(|scope| !authorized_scopes.contains(scope.as_str()))
            || request_scopes.as_ref() != Some(&declared_scopes)
        {
            finding(
                findings,
                "g0-access-scope-purpose",
                &access.repository,
                "collector_snapshot.access.scopes/raw_object_refs",
                "each per-repository permission must be authorized by collector.auth and backed by that repository's exact captured request purpose",
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
            &raw_ids,
            findings,
        );
    }

    for request in collector.requests.iter().filter(|request| {
        request.api == G0ApiKind::Rest && request.endpoint_or_operation.starts_with("/repos/")
    }) {
        let Some(repository) = g0_rest_request_repository(&request.endpoint_or_operation) else {
            finding(
                findings,
                "g0-access-scope-purpose",
                "",
                "collector_snapshot.requests.endpoint_or_operation",
                "repository API requests must identify one canonical repository for access-scope binding",
            );
            continue;
        };
        let matching_access = collector
            .access
            .iter()
            .filter(|access| access.repository == repository)
            .collect::<Vec<_>>();
        let Some(access) = matching_access
            .first()
            .filter(|_| matching_access.len() == 1)
        else {
            finding(
                findings,
                "g0-access-scope-purpose",
                &repository,
                "collector_snapshot.requests.endpoint_or_operation",
                "every repository API request must join exactly one repository access observation",
            );
            continue;
        };
        let Some(required_scope_options) = g0_repository_request_scopes(request, &repository)
        else {
            finding(
                findings,
                "g0-access-scope-purpose",
                &repository,
                "collector_snapshot.requests.endpoint_or_operation",
                "repository API request endpoint has no authorized read-purpose mapping",
            );
            continue;
        };
        let access_scopes = access
            .scopes
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !required_scope_options
            .iter()
            .any(|scope| access_scopes.contains(scope))
        {
            finding(
                findings,
                "g0-access-scope-purpose",
                &repository,
                "collector_snapshot.access.scopes/requests",
                format!(
                    "request {} requires a read scope absent from this repository's authorized access observation",
                    request.endpoint_or_operation
                ),
            );
        }
    }
}

fn g0_access_request_scopes(
    access: &G0AccessObservation,
    collector: &G0CollectorSnapshot,
) -> Option<BTreeSet<&'static str>> {
    let mut scopes = BTreeSet::new();
    let declared_scopes = access
        .scopes
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut seen_raw_ids = BTreeSet::new();
    for raw_id in &access.raw_object_refs {
        if !seen_raw_ids.insert(raw_id.as_str()) {
            return None;
        }
        let mut matching_raw_objects = collector
            .raw_objects
            .iter()
            .filter(|raw| raw.raw_id == *raw_id);
        let raw = matching_raw_objects.next()?;
        if matching_raw_objects.next().is_some() {
            return None;
        }
        let mut matching_requests = collector
            .requests
            .iter()
            .filter(|request| request.request_id == raw.request_id);
        let request = matching_requests.next()?;
        if matching_requests.next().is_some() || request.response_raw_ref != raw.raw_id {
            return None;
        }
        let purpose_scopes = g0_repository_request_scopes(request, &access.repository)?;
        let matching_scopes = purpose_scopes
            .iter()
            .filter(|scope| declared_scopes.contains(**scope))
            .copied()
            .collect::<Vec<_>>();
        if matching_scopes.is_empty() {
            return None;
        }
        scopes.extend(matching_scopes);
    }
    Some(scopes)
}

fn g0_repository_request_scopes(
    request: &G0RequestRecord,
    repository: &str,
) -> Option<BTreeSet<&'static str>> {
    if request.api != G0ApiKind::Rest || request.method != "GET" {
        return None;
    }
    let repository_endpoint = format!("/repos/{repository}");
    let endpoint_suffix = request
        .endpoint_or_operation
        .strip_prefix(&repository_endpoint)?;
    if endpoint_suffix.is_empty() {
        return Some(BTreeSet::from(["metadata:read"]));
    }
    let parts = endpoint_suffix
        .strip_prefix('/')?
        .split('/')
        .collect::<Vec<_>>();
    let one = |scope| BTreeSet::from([scope]);
    match parts.as_slice() {
        ["contents"] | ["contents", ..] => Some(one("contents:read")),
        ["actions", "runners", ..] | ["actions", "permissions", ..] => {
            Some(one("administration:read"))
        }
        ["actions", "runs", ..] | ["actions", "workflows", ..] => Some(one("actions:read")),
        ["actions", "artifacts", artifact_id, "zip"]
            if artifact_id.parse::<u64>().is_ok_and(|value| value > 0) =>
        {
            Some(one("actions:read"))
        }
        ["pulls"] => Some(one("pull_requests:read")),
        ["pulls", number] if number.parse::<u64>().is_ok_and(|value| value > 0) => {
            Some(BTreeSet::from(["contents:read", "pull_requests:read"]))
        }
        ["pulls", number, "commits" | "files" | "merge"]
            if number.parse::<u64>().is_ok_and(|value| value > 0) =>
        {
            Some(one("pull_requests:read"))
        }
        ["commits", _, "status" | "statuses"] => Some(one("statuses:read")),
        ["commits", _, "check-runs" | "check-suites"] | ["check-suites", _, "check-runs"] => {
            Some(one("checks:read"))
        }
        ["branches", _, "protection", "required_status_checks"] => Some(one("administration:read")),
        ["rules", "branches", branch] if !branch.is_empty() => Some(one("metadata:read")),
        ["rulesets", "rule-suites"] => Some(one("administration:read")),
        ["rulesets"] | ["rulesets", _] => Some(one("metadata:read")),
        _ => None,
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
    if !run_url_matches_repository(&execution.run_url, repository, execution.run_id)
        || execution.workflow_path.trim().is_empty()
        || !valid_sha(&execution.workflow_revision)
    {
        finding(
            findings,
            "run-identity",
            repository,
            "execution",
            "canonical repository/run URL, workflow path, and immutable workflow revision are required",
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
    let mut check_contexts = BTreeMap::<&str, &str>::new();
    for check in &execution.required_checks {
        if check.context.trim().is_empty()
            || check.app_id.trim().is_empty()
            || check_contexts
                .insert(&check.context, &check.app_id)
                .is_some()
        {
            finding(
                findings,
                "check-duplicate",
                repository,
                "execution.required_checks",
                "required-check rows must have unique non-empty contexts and App identities",
            );
        }
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
    for child in &execution.child_workflows {
        if child.parent_run_id == 0
            || child.parent_run_attempt == 0
            || child.parent_repository.trim().is_empty()
            || !g0_workflow_file_path(&child.parent_workflow_path)
            || !valid_sha(&child.parent_source_sha)
            || child.run_id == 0
            || child.run_attempt == 0
            || !matches!(
                child.event.as_str(),
                "workflow_call" | "workflow_dispatch" | "workflow_run"
            )
            || !g0_child_workflow_run_identity_is_valid(child, repository, execution)
            || child.status != "completed"
            || child.conclusion != "success"
            || !g0_child_workflow_run_url_is_valid(child, repository, execution)
        {
            finding(
                findings,
                "child-run-conclusion",
                repository,
                "execution.child_workflows",
                "child workflow must use its caller's run identity for workflow_call and a distinct run identity for separate events",
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
    workflow_derivation: &mut WorkflowDerivationContext,
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
        check_execution_record(
            stage,
            index,
            record,
            g0_inventory,
            workflow_derivation,
            findings,
        );
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
    workflow_derivation: &mut WorkflowDerivationContext,
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
    check_authoritative_children_from_source_with_context(
        index.manifest,
        g0_inventory,
        record,
        execution,
        workflow_derivation,
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
    if execution.jobs.len() != expected_ids.len()
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
    // Actions Runner stores the workflow job's display name separately from
    // its job name/ref. This schema has only the provider run-job ID and
    // display name; it has no source job ID or authenticated raw-response
    // join. Never treat `job_name` as the source workflow's `job_id`.
    finding(
        findings,
        "job-source-join-unverified",
        repo,
        "execution.jobs.job_id/job_name",
        "run-specific provider job IDs and display names cannot be joined to source logical job IDs until the collector provides an authenticated source-job ID mapping",
    );
    let conclusion_ids = record
        .actual_job_conclusions
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if conclusion_ids != actual_ids {
        finding(
            findings,
            "job-conclusion",
            repo,
            "actual_job_conclusions",
            "record job conclusions must cover exactly the provider job IDs in the run",
        );
    }
    for actual in &execution.jobs {
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
                    "record lacks a successful conclusion for provider job {}",
                    actual.job_id,
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
    let mut observed_rows = execution
        .required_checks
        .iter()
        .map(|check| {
            (
                check.context.clone(),
                check.app_id.clone(),
                check.job_id.clone(),
                check.status.clone(),
                check.conclusion.clone(),
                check.run_id,
                check.source_url.clone(),
                check.event.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut claimed_rows = record
        .required_checks
        .iter()
        .map(|check| {
            (
                check.context.clone(),
                check.app_id.clone(),
                check.job_id.clone(),
                check.status.clone(),
                check.conclusion.clone(),
                check.run_id,
                check.source_url.clone(),
                check.event.clone(),
            )
        })
        .collect::<Vec<_>>();
    observed_rows.sort_unstable();
    claimed_rows.sort_unstable();
    if observed_rows != claimed_rows {
        finding(
            findings,
            "check-observation-mismatch",
            repo,
            "required_checks",
            "record required-check rows must match observed execution-check rows field-for-field and one-to-one",
        );
    }
    let mut claimed_contexts = BTreeMap::<&str, &str>::new();
    for check in &record.required_checks {
        if claimed_contexts
            .insert(&check.context, &check.app_id)
            .is_some()
        {
            finding(
                findings,
                "check-context-mismatch",
                repo,
                "required_checks",
                format!(
                    "required check context {} has duplicate or conflicting App identities",
                    check.context
                ),
            );
        }
    }
    if claimed.len() != record.required_checks.len()
        || claimed_contexts.len() != record.required_checks.len()
    {
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
        if execution
            .jobs
            .iter()
            .filter(|job| job.job_id == check.job_id)
            .count()
            != 1
        {
            finding(
                findings,
                "check-job-association",
                repo,
                "required_checks.job_id",
                format!(
                    "required check {} must name exactly one actual job",
                    check.context
                ),
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
    let mut workflow_derivation = WorkflowDerivationContext::default();
    check_authoritative_children_from_source_with_context(
        manifest,
        inventory,
        record,
        execution,
        &mut workflow_derivation,
        findings,
    );
}

fn check_authoritative_children_from_source_with_context(
    manifest: &ManifestRepository,
    inventory: Option<&G0InventoryEvidence>,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    workflow_derivation: &mut WorkflowDerivationContext,
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
    if !g0_workflow_inventory_sources_size_within_limit(workflow) {
        finding(
            findings,
            "authoritative-child-source-invalid",
            repo,
            "evidence.g0_inventory.collector_snapshot.repositories.workflows",
            "immutable workflow source or dependency exceeds the one MiB checker input limit",
        );
        return;
    }
    let dependencies = workflow
        .reusable_workflows
        .iter()
        .chain(workflow.actions.iter())
        .chain(workflow.scanners.iter())
        .cloned()
        .collect::<Vec<_>>();
    let plan = match derive_workflow_plan_with_context(
        &workflow.source,
        &dependencies,
        workflow_derivation,
    ) {
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
    // G0 binds all observed root runs; repeat the check at the evidence join so
    // this record remains source-bound when checked on its own.
    check_g0_root_execution_event(repo, &workflow.source, &plan, execution, findings);
    let expected = plan
        .child_edges
        .iter()
        .filter(|edge| g0_child_edge_is_supported(edge))
        .filter(|edge| {
            manifest.expected_jobs.iter().any(|job| {
                job.job_id == edge.root_workload_id
                    && job.provider == record.provider
                    && job.required
            })
        })
        .collect::<Vec<_>>();
    let mut matched_edges = BTreeSet::new();
    let mut observed_child_identities = BTreeSet::new();
    if expected.len() != execution.child_workflows.len()
        || expected.len() != record.child_workflow_links.len()
    {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "execution.child_workflows",
            "child-run graph must exactly cover source-derived recursive workflow obligations",
        );
    }
    let edge_match_context = G0ChildEdgeMatchContext {
        repository: &repository.repository,
        provider: &record.provider,
        execution,
        root_source: &workflow.source,
        manifest,
        plan: &plan,
        reusable_workflows: &workflow.reusable_workflows,
    };
    for child in &execution.child_workflows {
        if !observed_child_identities.insert(g0_child_workflow_observation_identity(child)) {
            finding(
                findings,
                "duplicate-child-run",
                repo,
                "execution.child_workflows",
                format!(
                    "duplicate child workflow membership {} at {}",
                    child.workflow_path, child.source_sha
                ),
            );
        }
        let matching_edges = expected
            .iter()
            .enumerate()
            .filter(|(_, edge)| g0_child_edge_matches_observation(child, edge, &edge_match_context))
            .collect::<Vec<_>>();
        let edge_is_unique = matching_edges.len() == 1
            && matching_edges
                .first()
                .is_some_and(|(index, _)| matched_edges.insert(*index));
        if !edge_is_unique
            || child.provider != record.provider
            || child.parent_run_attempt == 0
            || child.run_id == 0
            || child.run_attempt == 0
            || !g0_child_workflow_run_identity_is_valid(child, &repository.repository, execution)
            || child.status != "completed"
            || child.conclusion != "success"
            || !valid_sha(&child.source_sha)
            || !g0_child_workflow_run_url_is_valid(child, &repository.repository, execution)
        {
            finding(
                findings,
                "child-run-mismatch",
                repo,
                "execution.child_workflows",
                format!(
                    "child workflow {} lacks exact parent, source, event, status, or URL binding",
                    child.workflow_path
                ),
            );
        }
    }
    if matched_edges.len() != expected.len() {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "execution.child_workflows",
            "each source-derived child edge must map to exactly one observed workflow membership",
        );
    }
    let mut child_workflow_identities = BTreeSet::new();
    for link in &record.child_workflow_links {
        if !child_workflow_identities.insert(g0_child_workflow_link_identity(link)) {
            finding(
                findings,
                "duplicate-child-run",
                repo,
                "child_workflow_links",
                format!(
                    "duplicate child workflow membership {} at {}",
                    link.workflow_path, link.source_sha
                ),
            );
        }
        let mut matching_actual = execution.child_workflows.iter().filter(|run| {
            g0_child_workflow_observation_identity(run) == g0_child_workflow_link_identity(link)
        });
        let Some(actual) = matching_actual.next() else {
            finding(
                findings,
                "missing-child-run",
                repo,
                "child_workflow_links",
                format!(
                    "child workflow {} is absent from the observed workflow graph",
                    link.workflow_path
                ),
            );
            continue;
        };
        if matching_actual.next().is_some()
            || link.provider != actual.provider
            || link.status != actual.status
            || link.conclusion != actual.conclusion
            || link.run_url != actual.source_url
        {
            finding(
                findings,
                "child-run-mismatch",
                repo,
                "child_workflow_links",
                format!(
                    "child workflow {} claim differs from the observed workflow graph",
                    link.workflow_path
                ),
            );
        }
    }
    let actual_child_identities = execution
        .child_workflows
        .iter()
        .map(g0_child_workflow_observation_identity)
        .collect::<BTreeSet<_>>();
    if child_workflow_identities != actual_child_identities {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "child_workflow_links",
            "child workflow links must form a complete bijection with observed workflow memberships",
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
    let producer_is_record_run = g0_release_producer_is_record_run(&execution.producer, record);
    let producer_child_link_matches = record
        .child_workflow_links
        .iter()
        .filter(|link| g0_release_producer_matches_child_link(&execution.producer, link))
        .count();
    if !producer_is_record_run && producer_child_link_matches != 1 {
        finding(
            findings,
            "producer-mismatch",
            repo,
            "release.execution.producer.run_id",
            "producer must exactly match the record run or one repository/workflow/source/URL-bound child link; the source record does not carry a run attempt",
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
    let mut seen_contexts = BTreeMap::<&str, &str>::new();
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
        if seen_contexts
            .insert(&context.context, &context.app_id)
            .is_some()
        {
            finding(
                findings,
                "check-context",
                repository,
                "required_check_contexts_and_apps",
                format!(
                    "required context {} has duplicate or conflicting App identities",
                    context.context
                ),
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
    use std::io::Write;

    fn sha(seed: char) -> String {
        std::iter::repeat_n(seed, SHA_LENGTH).collect()
    }

    fn digest(seed: char) -> String {
        format!(
            "sha256:{}",
            std::iter::repeat_n(seed, DIGEST_LENGTH).collect::<String>()
        )
    }

    fn generated_state_archive(
        source_url: &str,
        source_revision: &str,
        source_digest: &str,
    ) -> Vec<u8> {
        let payload = json!({
            "schema": G0_GENERATED_STATE_SCHEMA,
            "source_url": source_url,
            "source_revision": source_revision,
            "source_digest": source_digest,
        });
        generated_state_archive_payload(&payload)
    }

    fn generated_state_archive_payload(payload: &Value) -> Vec<u8> {
        let payload_bytes = canonical_json(payload).into_bytes();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file(
                G0_GENERATED_STATE_ARCHIVE_MEMBER,
                zip::write::FileOptions::<()>::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(&payload_bytes).unwrap();
        archive.finish().unwrap().into_inner()
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

    /// Complete typed collector fixture.  It is intentionally generated from
    /// the checker-owned canonical scope, with every repository, current PR,
    /// workflow, required check, raw object, reconciliation row, graph edge,
    /// model observation, and access observation populated.  It is a positive
    /// contract fixture, not a claim about the live fleet.
    fn complete_g0_fixture() -> (ManifestDocument, SnapshotDocument, G0InventoryEvidence) {
        let mut requests = Vec::with_capacity(REQUIRED_REPOSITORIES * 4);
        let mut raw_objects = Vec::with_capacity(REQUIRED_REPOSITORIES * 4);
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
            let workflow_raw_id = format!("raw-workflow-{repository_id}");
            let workflow_request_id = format!("request-workflow-{repository_id}");
            let generated_state_raw_id = format!("raw-generated-state-{repository_id}");
            let generated_state_request_id = format!("request-generated-state-{repository_id}");
            let generated_state_name = format!("generated-state-{repository_id}");
            let raw_bytes =
                format!("{{\"repository\":\"{repository}\",\"id\":{repository_id}}}").into_bytes();
            let raw_digest = digest_bytes(&raw_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: raw_id.clone(),
                request_id: request_id.clone(),
                object_kind: "github-response".to_owned(),
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
            let artifact_bytes =
                format!("{{\"artifact_id\":{artifact_id},\"run_id\":{main_run_id}}}").into_bytes();
            let artifact_digest = digest_bytes(&artifact_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: artifact_raw_id.clone(),
                request_id: artifact_request_id.clone(),
                object_kind: "workflow_artifacts".to_owned(),
                canonicalization: "raw-json".to_owned(),
                sha256: artifact_digest.clone(),
                byte_length: artifact_bytes.len() as u64,
                bytes_base64: BASE64.encode(&artifact_bytes),
                media_type: "application/json".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    artifact_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                original_sha256: artifact_digest.clone(),
                original_byte_length: artifact_bytes.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    artifact_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
            });
            let workflow_raw_digest = digest_bytes(workflow_bytes);
            let workflow_source_url =
                format!("https://github.com/{repository}/blob/{source_sha}/{workflow_path}");
            let generated_state_bytes =
                generated_state_archive(&workflow_source_url, &source_sha, &workflow_raw_digest);
            let generated_state_digest = digest_bytes(&generated_state_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: workflow_raw_id.clone(),
                request_id: workflow_request_id.clone(),
                object_kind: "github-workflow-source".to_owned(),
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
            raw_objects.push(G0RawObjectRef {
                raw_id: generated_state_raw_id.clone(),
                request_id: generated_state_request_id.clone(),
                object_kind: "workflow_artifact_archive".to_owned(),
                canonicalization: "identity".to_owned(),
                sha256: generated_state_digest.clone(),
                byte_length: generated_state_bytes.len() as u64,
                bytes_base64: BASE64.encode(&generated_state_bytes),
                media_type: "application/zip".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    generated_state_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                original_sha256: generated_state_digest.clone(),
                original_byte_length: generated_state_bytes.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    generated_state_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
            });
            requests.push(G0RequestRecord {
                request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: format!("/repos/{repository}"),
                accept: "application/vnd.github+json".to_owned(),
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
                    per_page: None,
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
                accept: "application/vnd.github+json".to_owned(),
                query_base64: BASE64.encode(b"page=1&per_page=100"),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(b"page=1&per_page=100"),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-artifact-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: Some(100),
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
                endpoint_or_operation: format!("/repos/{repository}/contents/{workflow_path}"),
                accept: "application/vnd.github.raw+json".to_owned(),
                query_base64: BASE64.encode(format!("ref={source_sha}").as_bytes()),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(format!("ref={source_sha}").as_bytes()),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-workflow-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: None,
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
            requests.push(G0RequestRecord {
                request_id: generated_state_request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: format!(
                    "/repos/{repository}/actions/artifacts/{artifact_id}/zip"
                ),
                accept: "application/vnd.github+json".to_owned(),
                query_base64: BASE64.encode(b""),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(b""),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-generated-state-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: None,
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: false,
                    items_returned: 1,
                },
                response_raw_ref: generated_state_raw_id.clone(),
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
            // Keep one real external-App shape in the positive fixture while
            // exercising the captured GitHub Actions shape for the rest.
            let provider = if index == 0 {
                G0CheckProvider::ExternalApp
            } else {
                G0CheckProvider::GithubActions
            };
            let app_slug = if index == 0 {
                "dco-2"
            } else {
                "github-actions"
            };
            let main_check_url = format!("{repository_url}/runs/{main_check_run_id}");
            let pr_check_url = format!("{repository_url}/runs/{pr_check_run_id}");
            let main_job_url = format!("{repository_url}/runs/{main_run_id}/jobs/{main_job_id}");
            let pr_job_url = format!("{repository_url}/runs/{pr_run_id}/jobs/{pr_job_id}");
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
                child_workflows: Vec::new(),
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
                child_workflows: Vec::new(),
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
                name: generated_state_name.clone(),
                schema: G0_GENERATED_STATE_SCHEMA.to_owned(),
                source_url: workflow_source_url.clone(),
                sha256: generated_state_digest.clone(),
                storage_ref: format!(
                    "sha256://{}",
                    generated_state_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                source_revision: source_sha.clone(),
                source_digest: workflow_raw_digest.clone(),
                observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                raw_object_refs: vec![generated_state_raw_id.clone()],
            };
            let workflow_source = G0WorkflowSource {
                repository: repository.clone(),
                path: workflow_path.to_owned(),
                revision: source_sha.clone(),
                source_sha: source_sha.clone(),
                source_url: workflow_source_url,
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
            let main_check_producer = G0CheckProducer {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                provider,
                app_slug: app_slug.to_owned(),
                check_suite_id: main_check_suite_id,
                check_run_id: main_check_run_id,
                workflow_run_id: main_run_id,
                run_attempt: 1,
                job_id: main_job_id,
                job_run_id: main_run_id,
                job_run_attempt: 1,
                job_check_run_id: main_check_run_id,
                job_source_sha: source_sha.clone(),
                job_html_url: main_job_url,
                source_sha: source_sha.clone(),
                actual_checkout_sha: source_sha.clone(),
                event: "push".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                html_url: main_check_url,
                raw_object_refs: vec![raw_id.clone()],
            };
            let pr_check_producer = G0CheckProducer {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                provider,
                app_slug: app_slug.to_owned(),
                check_suite_id: pr_check_suite_id,
                check_run_id: pr_check_run_id,
                workflow_run_id: pr_run_id,
                run_attempt: 1,
                job_id: pr_job_id,
                job_run_id: pr_run_id,
                job_run_attempt: 1,
                job_check_run_id: pr_check_run_id,
                job_source_sha: head_sha.clone(),
                job_html_url: pr_job_url,
                source_sha: head_sha.clone(),
                actual_checkout_sha: merge_sha.clone(),
                event: "pull_request".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                html_url: pr_check_url,
                raw_object_refs: vec![raw_id.clone()],
            };
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
                    run_head_sha: source_sha.clone(),
                    name: generated_state_name,
                    // The API listing body hash is not the artifact archive digest.
                    digest: generated_state_digest,
                    expired: false,
                    source_url: format!(
                        "https://api.github.com/repos/{repository}/actions/artifacts/{artifact_id}/zip"
                    ),
                    raw_object_refs: vec![artifact_raw_id.clone()],
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
                    tested_merge_sha: Some(merge_sha.clone()),
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
                        runs: vec![G0WorkflowRunIdentity {
                            run_id: pr_run_id,
                            run_attempt: 1,
                        }],
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
                tested_merge_sha: Some(merge_sha.clone()),
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
                scopes: vec![
                    "actions:read".to_owned(),
                    "contents:read".to_owned(),
                    "metadata:read".to_owned(),
                ],
                gaps: Vec::new(),
                raw_object_refs: vec![
                    raw_id.clone(),
                    artifact_raw_id.clone(),
                    workflow_raw_id.clone(),
                ],
            });
        }
        let nodes = CANONICAL_REPOSITORIES
            .iter()
            .flat_map(|repository| {
                [
                    G0GraphNode {
                        id: format!("workload:{repository}:scan"),
                        kind: "workload".to_owned(),
                        repository: (*repository).to_owned(),
                        workload_id: "scan".to_owned(),
                        applicability: "required".to_owned(),
                        source_sha: source_sha.clone(),
                        source_ref: "refs/heads/main".to_owned(),
                        raw_object_refs: vec!["raw-repository-1".to_owned()],
                    },
                    G0GraphNode {
                        id: format!("check:{repository}:ci"),
                        kind: "check".to_owned(),
                        repository: (*repository).to_owned(),
                        workload_id: "scan".to_owned(),
                        applicability: "required".to_owned(),
                        source_sha: source_sha.clone(),
                        source_ref: "refs/heads/main".to_owned(),
                        raw_object_refs: vec!["raw-repository-1".to_owned()],
                    },
                ]
            })
            .collect::<Vec<_>>();
        let edges = CANONICAL_REPOSITORIES
            .iter()
            .map(|repository| G0GraphEdge {
                from: format!("workload:{repository}:scan"),
                to: format!("check:{repository}:ci"),
                kind: "workload-to-check".to_owned(),
                required: true,
                source_sha: source_sha.clone(),
                source_ref: "refs/heads/main".to_owned(),
                target_source_sha: source_sha.clone(),
                target_source_ref: "refs/heads/main".to_owned(),
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            })
            .collect::<Vec<_>>();
        let first_raw_digest = raw_objects
            .first()
            .map(|raw| raw.sha256.clone())
            .expect("complete fixture has a first raw object");
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
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            },
            model_session: G0ModelSession {
                session_id: "session".to_owned(),
                effective: true,
                orchestrator_model: EXPECTED_ORCHESTRATOR_MODEL.to_owned(),
                orchestrator_effort: EXPECTED_ORCHESTRATOR_EFFORT.to_owned(),
                agents: vec![G0AgentModel {
                    agent_id: "agent".to_owned(),
                    model: EXPECTED_AGENT_MODEL.to_owned(),
                    effort: EXPECTED_AGENT_EFFORT.to_owned(),
                    effective: true,
                    raw_object_refs: vec!["raw-repository-1".to_owned()],
                }],
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            },
            access,
            workload_artifact: G0ArtifactReference {
                name: "workload-matrix".to_owned(),
                schema: "velnor.workload-matrix.v1".to_owned(),
                source_url: "https://github.com/tailrocks/velnor/blob/reviewed/matrix.json"
                    .to_owned(),
                sha256: first_raw_digest.clone(),
                storage_ref: format!(
                    "sha256://{}",
                    first_raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                source_revision: source_sha,
                source_digest: digest('c'),
                observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            },
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

    fn replace_g0_workflow_source_bytes(
        inventory: &mut G0InventoryEvidence,
        repository_index: usize,
        workflow_bytes: &[u8],
    ) {
        let workflow =
            &mut inventory.collector_snapshot.repositories[repository_index].workflows[0];
        let source = &mut workflow.source;
        let digest = digest_bytes(workflow_bytes);
        source.byte_length = workflow_bytes.len() as u64;
        source.bytes_base64 = BASE64.encode(workflow_bytes);
        source.sha256 = digest.clone();
        let source_url = source.source_url.clone();
        let source_revision = source.revision.clone();
        source.storage_ref = format!(
            "sha256://{}",
            digest.strip_prefix("sha256:").expect("digest has prefix")
        );
        let raw_id = source.raw_object_refs[0].clone();
        let raw = inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == raw_id)
            .expect("workflow source raw object");
        raw.byte_length = workflow_bytes.len() as u64;
        raw.bytes_base64 = BASE64.encode(workflow_bytes);
        raw.sha256 = digest.clone();
        raw.storage_ref = format!(
            "sha256://{}",
            digest.strip_prefix("sha256:").expect("digest has prefix")
        );
        raw.original_byte_length = workflow_bytes.len() as u64;
        raw.original_sha256 = digest;
        raw.original_storage_ref = raw.storage_ref.clone();

        let generated_state = &mut inventory.collector_snapshot.repositories[repository_index]
            .workflows[0]
            .generated_state;
        generated_state.source_digest = digest_bytes(workflow_bytes);
        let generated_state_bytes = generated_state_archive(
            &source_url,
            &source_revision,
            &generated_state.source_digest,
        );
        let generated_state_digest = digest_bytes(&generated_state_bytes);
        generated_state.sha256 = generated_state_digest.clone();
        generated_state.storage_ref = format!(
            "sha256://{}",
            generated_state_digest
                .strip_prefix("sha256:")
                .expect("digest has prefix")
        );
        let generated_state_raw_id = generated_state.raw_object_refs[0].clone();
        let generated_state_name = generated_state.name.clone();
        let generated_state_raw = inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == generated_state_raw_id)
            .expect("generated-state archive raw object");
        generated_state_raw.byte_length = generated_state_bytes.len() as u64;
        generated_state_raw.bytes_base64 = BASE64.encode(&generated_state_bytes);
        generated_state_raw.sha256 = generated_state_digest.clone();
        generated_state_raw.storage_ref = format!(
            "sha256://{}",
            generated_state_digest
                .strip_prefix("sha256:")
                .expect("digest has prefix")
        );
        generated_state_raw.original_byte_length = generated_state_bytes.len() as u64;
        generated_state_raw.original_sha256 = generated_state_digest.clone();
        generated_state_raw.original_storage_ref = generated_state_raw.storage_ref.clone();
        inventory.collector_snapshot.repositories[repository_index]
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.name == generated_state_name)
            .expect("generated-state artifact row")
            .digest = generated_state_digest;
        refresh_typed_inventory_bytes(inventory);
    }

    fn replace_g0_generated_state_archive_bytes(
        inventory: &mut G0InventoryEvidence,
        repository_index: usize,
        archive_bytes: &[u8],
    ) {
        let archive_digest = digest_bytes(archive_bytes);
        let (raw_id, artifact_name) = {
            let generated_state = &mut inventory.collector_snapshot.repositories[repository_index]
                .workflows[0]
                .generated_state;
            generated_state.sha256 = archive_digest.clone();
            generated_state.storage_ref = format!(
                "sha256://{}",
                archive_digest
                    .strip_prefix("sha256:")
                    .expect("digest has prefix")
            );
            (
                generated_state.raw_object_refs[0].clone(),
                generated_state.name.clone(),
            )
        };
        let raw = inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == raw_id)
            .expect("generated-state archive raw object");
        raw.byte_length = archive_bytes.len() as u64;
        raw.bytes_base64 = BASE64.encode(archive_bytes);
        raw.sha256 = archive_digest.clone();
        raw.storage_ref = format!(
            "sha256://{}",
            archive_digest
                .strip_prefix("sha256:")
                .expect("digest has prefix")
        );
        raw.original_byte_length = archive_bytes.len() as u64;
        raw.original_sha256 = archive_digest.clone();
        raw.original_storage_ref = raw.storage_ref.clone();
        inventory.collector_snapshot.repositories[repository_index]
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.name == artifact_name)
            .expect("generated-state artifact row")
            .digest = archive_digest;
        refresh_typed_inventory_bytes(inventory);
    }

    #[test]
    fn artifact_names_may_repeat_for_distinct_runs() {
        let (_, _, inventory) = complete_g0_fixture();
        let collector = &inventory.collector_snapshot;
        let repository_inventory = &collector.repositories[0];
        let repository = &repository_inventory.repository;
        let original = repository_inventory.artifacts[0].clone();
        let producer = &repository_inventory.open_prs[0].required_check_producers[0];
        let artifact_id = original.artifact_id + 1_000_000;
        let request_id = "request-artifact-repeat".to_owned();
        let raw_id = "raw-artifact-repeat".to_owned();

        let mut repeated = original.clone();
        repeated.artifact_id = artifact_id;
        repeated.run_id = producer.workflow_run_id;
        repeated.run_head_sha = producer.source_sha.clone();
        repeated.digest = digest('e');
        repeated.source_url = format!(
            "https://api.github.com/repos/{repository}/actions/artifacts/{artifact_id}/zip"
        );
        repeated.raw_object_refs = vec![raw_id.clone()];

        let mut request = collector
            .requests
            .iter()
            .find(|request| request.response_raw_ref == original.raw_object_refs[0])
            .expect("complete fixture has original artifact request")
            .clone();
        request.request_id = request_id.clone();
        request.api_request_id = "api-artifact-repeat".to_owned();
        request.endpoint_or_operation = format!(
            "/repos/{repository}/actions/runs/{}/artifacts",
            producer.workflow_run_id
        );
        request.query_base64 = BASE64.encode(b"page=1&per_page=100");
        request.query_sha256 = digest_bytes(b"page=1&per_page=100");
        request.response_raw_ref = raw_id.clone();

        let response_bytes = format!(
            "{{\"artifacts\":[{{\"id\":{artifact_id},\"name\":\"{}\",\"workflow_run\":{{\"id\":{},\"head_sha\":\"{}\"}}}}]}}",
            original.name, producer.workflow_run_id, producer.source_sha
        )
        .into_bytes();
        let response_digest = digest_bytes(&response_bytes);
        let mut raw = collector
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == original.raw_object_refs[0])
            .expect("complete fixture has original artifact response")
            .clone();
        raw.raw_id = raw_id;
        raw.request_id = request_id;
        raw.bytes_base64 = BASE64.encode(&response_bytes);
        raw.byte_length = response_bytes.len() as u64;
        raw.sha256 = response_digest.clone();
        raw.storage_ref = format!(
            "sha256://{}",
            response_digest
                .strip_prefix("sha256:")
                .expect("response digest has prefix")
        );
        raw.original_sha256 = response_digest.clone();
        raw.original_byte_length = response_bytes.len() as u64;
        raw.original_storage_ref = raw.storage_ref.clone();

        let mut artifacts = repository_inventory.artifacts.clone();
        artifacts.push(repeated);
        let mut requests = collector.requests.clone();
        requests.push(request);
        let mut raw_objects = collector.raw_objects.clone();
        raw_objects.push(raw);
        let raw_ids = raw_objects
            .iter()
            .map(|raw| raw.raw_id.clone())
            .collect::<BTreeSet<_>>();
        let mut findings = Vec::new();
        check_g0_artifact_observations(
            repository,
            &artifacts,
            G0ArtifactContext {
                default_branch_sha: &repository_inventory.default_branch_sha,
                open_prs: &repository_inventory.open_prs,
                main_checks: &repository_inventory.main_checks,
                requests: &requests,
                raw_ids: &raw_ids,
                raw_objects: &raw_objects,
            },
            &mut findings,
        );
        assert!(
            !g0_codes(&findings).contains("g0-artifact-identity"),
            "same-name artifacts from separate runs are valid: {findings:?}"
        );
        assert!(
            !g0_codes(&findings).contains("g0-artifact-request"),
            "repeated artifact row should retain its exact run request binding: {findings:?}"
        );
    }

    #[test]
    fn g0_structural_fixture_is_consistent_and_mutations_fail() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");

        let mut truncated = inventory.clone();
        truncated.collector_snapshot.requests[0].truncation_reason =
            Some("response ended before census completed".to_owned());
        refresh_typed_inventory_bytes(&mut truncated);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&truncated), &mut findings);
        assert!(g0_codes(&findings).contains("g0-request-incomplete"));

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
        check.html_url = format!(
            "https://github.com/{}/runs/{}",
            CANONICAL_REPOSITORIES[0],
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

        let mut wrong_actions_check = inventory.clone();
        let action_check =
            &mut wrong_actions_check.collector_snapshot.repositories[1].main_checks[0];
        action_check.html_url = format!(
            "https://github.com/{}/runs/{}",
            CANONICAL_REPOSITORIES[1],
            action_check.check_run_id + 1
        );
        assert_check_url_rejected!(wrong_actions_check);

        let mut wrong_actions_job_run = inventory.clone();
        let action_check =
            &mut wrong_actions_job_run.collector_snapshot.repositories[1].main_checks[0];
        action_check.job_html_url = format!(
            "https://github.com/{}/runs/{}/jobs/{}",
            CANONICAL_REPOSITORIES[1],
            action_check.workflow_run_id + 1,
            action_check.job_id
        );
        assert_check_url_rejected!(wrong_actions_job_run);

        let mut wrong_actions_job = inventory.clone();
        let action_check = &mut wrong_actions_job.collector_snapshot.repositories[1].main_checks[0];
        action_check.job_html_url = format!(
            "https://github.com/{}/runs/{}/jobs/{}",
            CANONICAL_REPOSITORIES[1],
            action_check.workflow_run_id,
            action_check.job_id + 1
        );
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

        let mut unsupported_artifact_attempt =
            serde_json::to_value(&inventory.collector_snapshot.repositories[0].artifacts[0])
                .expect("serialize artifact observation");
        assert!(unsupported_artifact_attempt.get("run_attempt").is_none());
        unsupported_artifact_attempt["run_attempt"] = json!(2);
        assert!(
            serde_json::from_value::<G0ArtifactObservation>(unsupported_artifact_attempt).is_err()
        );

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

        let mut wrong_pr_binding_attempt = inventory.clone();
        wrong_pr_binding_attempt.collector_snapshot.repositories[0].open_prs[0].workflow_bindings
            [0]
        .runs[0]
            .run_attempt = 2;
        refresh_typed_inventory_bytes(&mut wrong_pr_binding_attempt);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&wrong_pr_binding_attempt),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-pr-workflow-binding"));
        assert!(
            g0_codes(&findings).contains("g0-pr-check-producer")
                || g0_codes(&findings).contains("g0-pr-check-snapshot-mismatch")
        );

        let mut wrong_pr_binding_checkout = inventory.clone();
        wrong_pr_binding_checkout.collector_snapshot.repositories[0].open_prs[0]
            .workflow_bindings[0]
            .actual_checkout_sha = sha('z');
        refresh_typed_inventory_bytes(&mut wrong_pr_binding_checkout);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&wrong_pr_binding_checkout),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-pr-workflow-binding"));
        assert!(
            g0_codes(&findings).contains("g0-pr-check-producer")
                || g0_codes(&findings).contains("g0-pr-check-snapshot-mismatch")
        );

        let mut graph_tampered = inventory.clone();
        graph_tampered
            .collector_snapshot
            .dependency_graph
            .edges
            .clear();
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&graph_tampered), &mut findings);
        assert!(g0_codes(&findings).contains("g0-dependency-graph"));

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
        let bytes = b"on: [push]\njobs:\n  scan:\n    uses: ./.github/workflows/missing.yml\n";
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
    fn g0_pr_inventory_does_not_require_test_merge_sha() {
        let (manifest, mut snapshot, mut inventory) = complete_g0_fixture();
        for repository in &mut snapshot.repositories {
            for pull_request in &mut repository.open_prs {
                pull_request.merge_sha = None;
            }
        }
        for repository in &mut inventory.collector_snapshot.repositories {
            for pull_request in &mut repository.open_prs {
                pull_request.tested_merge_sha = None;
            }
        }
        for repository in inventory
            .collector_snapshot
            .reconciliation
            .pre_state
            .iter_mut()
            .chain(
                inventory
                    .collector_snapshot
                    .reconciliation
                    .post_state
                    .iter_mut(),
            )
        {
            for pull_request in &mut repository.prs {
                pull_request.tested_merge_sha = None;
            }
        }
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn g0_tested_merge_sha_matches_snapshot_option_exactly() {
        let (manifest, mut snapshot, inventory) = complete_g0_fixture();
        snapshot.repositories[0].open_prs[0].merge_sha = None;
        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-pr-snapshot-mismatch"));

        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        inventory.collector_snapshot.repositories[0].open_prs[0].tested_merge_sha = None;
        refresh_typed_inventory_bytes(&mut inventory);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-pr-snapshot-mismatch"));
    }

    #[test]
    fn pr_execution_must_match_one_captured_source_before_event_comparison() {
        let (manifest, mut snapshot, mut inventory) = complete_g0_fixture();
        let unbound_path = ".github/workflows/uncaptured.yml";
        let unbound_revision = sha('f');
        let execution = &mut snapshot.repositories[0].open_prs[0].executions[0];
        execution.workflow_path = unbound_path.to_owned();
        execution.workflow_revision = unbound_revision.clone();
        let binding =
            &mut inventory.collector_snapshot.repositories[0].open_prs[0].workflow_bindings[0];
        binding.workflow_path = unbound_path.to_owned();
        binding.workflow_revision = unbound_revision;
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-execution-workflow-source"),
            "matching PR snapshot and workflow-binding projections must still fail without captured source bytes: {findings:?}"
        );

        let mut unbound_execution = snapshot.repositories[0].open_prs[0].executions[0].clone();
        unbound_execution.event = "workflow_dispatch".to_owned();
        let workflow = &inventory.collector_snapshot.repositories[0].workflows[0];
        let dependencies = workflow
            .reusable_workflows
            .iter()
            .chain(workflow.actions.iter())
            .chain(workflow.scanners.iter())
            .cloned()
            .collect::<Vec<_>>();
        let plan = derive_workflow_plan(&workflow.source, &dependencies)
            .expect("fixture workflow source derives");
        let captured_sources = vec![(workflow, Some(plan))];
        findings.clear();
        check_g0_execution_workflow_source(
            &manifest.repositories[0].repository,
            &unbound_execution,
            &captured_sources,
            &mut findings,
        );
        assert_eq!(
            g0_codes(&findings),
            BTreeSet::from(["g0-execution-workflow-source".to_owned()]),
            "an unbound path/revision must fail before its event is compared with a different source"
        );
    }

    #[test]
    fn generated_state_must_bind_captured_source_run_and_artifact_bytes() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();

        let mut findings = Vec::new();
        for field in ["source_url", "source_revision", "source_digest"] {
            let mut wrong_source = inventory.clone();
            let artifact =
                &mut wrong_source.collector_snapshot.repositories[0].workflows[0].generated_state;
            match field {
                "source_url" => artifact.source_url.push_str("/uncaptured"),
                "source_revision" => artifact.source_revision = sha('f'),
                "source_digest" => artifact.source_digest = digest('e'),
                _ => artifact.source_digest = digest('e'),
            }
            refresh_typed_inventory_bytes(&mut wrong_source);
            findings.clear();
            check_g0_inventory(&manifest, &snapshot, Some(&wrong_source), &mut findings);
            assert!(
                g0_codes(&findings).contains("g0-generated-state-source"),
                "generated-state {field} must bind to captured source: {findings:?}"
            );
        }

        let source = &inventory.collector_snapshot.repositories[0].workflows[0].source;
        let mismatched_source_archive =
            generated_state_archive(&source.source_url, &source.revision, &digest('e'));
        let mut mismatched_source_bytes = inventory.clone();
        replace_g0_generated_state_archive_bytes(
            &mut mismatched_source_bytes,
            0,
            &mismatched_source_archive,
        );
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&mismatched_source_bytes),
            &mut findings,
        );
        assert!(
            g0_codes(&findings).contains("g0-generated-state-schema"),
            "measured archive bytes must attest the same source identity as the captured workflow: {findings:?}"
        );

        let artifact = &inventory.collector_snapshot.repositories[0].workflows[0].generated_state;
        let wrong_schema_payload = json!({
            "schema": "velnor.generated-state.unreviewed",
            "source_url": artifact.source_url,
            "source_revision": artifact.source_revision,
            "source_digest": artifact.source_digest,
        });
        let wrong_schema_archive = generated_state_archive_payload(&wrong_schema_payload);
        let mut wrong_schema = inventory.clone();
        replace_g0_generated_state_archive_bytes(&mut wrong_schema, 0, &wrong_schema_archive);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_schema), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-generated-state-schema"),
            "re-hashed artifact bytes with an unsupported schema must fail: {findings:?}"
        );

        let mut wrong_artifact = inventory.clone();
        let unrelated_raw = wrong_artifact.collector_snapshot.raw_objects[0].clone();
        let artifact =
            &mut wrong_artifact.collector_snapshot.repositories[0].workflows[0].generated_state;
        artifact.sha256 = unrelated_raw.sha256.clone();
        artifact.storage_ref = unrelated_raw.storage_ref.clone();
        artifact.raw_object_refs = vec![unrelated_raw.raw_id];
        wrong_artifact.collector_snapshot.repositories[0].artifacts[0].digest =
            unrelated_raw.sha256;
        refresh_typed_inventory_bytes(&mut wrong_artifact);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_artifact), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-generated-state-artifact"),
            "repository metadata bytes must not stand in for the downloaded artifact archive: {findings:?}"
        );
        assert!(
            !g0_codes(&findings).contains("g0-artifact-digest"),
            "the generic digest equality alone is insufficient to authenticate generated state: {findings:?}"
        );

        let mut wrong_endpoint = inventory.clone();
        let archive_raw_id = wrong_endpoint.collector_snapshot.repositories[0].workflows[0]
            .generated_state
            .raw_object_refs[0]
            .clone();
        let archive_request_id = wrong_endpoint
            .collector_snapshot
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == archive_raw_id)
            .expect("generated-state raw object")
            .request_id
            .clone();
        wrong_endpoint
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.request_id == archive_request_id)
            .expect("generated-state request")
            .endpoint_or_operation = format!("/repos/{}", CANONICAL_REPOSITORIES[0]);
        refresh_typed_inventory_bytes(&mut wrong_endpoint);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_endpoint), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-generated-state-artifact"),
            "archive bytes must come from the exact typed artifact download endpoint: {findings:?}"
        );

        let mut wrong_run = inventory.clone();
        wrong_run.collector_snapshot.repositories[0].artifacts[0].run_id += 1_000_000;
        refresh_typed_inventory_bytes(&mut wrong_run);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_run), &mut findings);
        assert!(g0_codes(&findings).contains("g0-generated-state-run"));
    }

    #[test]
    fn run_job_display_name_does_not_prove_source_job_identity() {
        let (manifest, _, _) = complete_g0_fixture();
        let manifest_repository = &manifest.repositories[0];
        let provider_job_id = "80123";
        let record = EvidenceRecord {
            repository: manifest_repository.repository.clone(),
            provider: "github".to_owned(),
            event: "push".to_owned(),
            expected_jobs: manifest_repository.expected_jobs.clone(),
            actual_job_ids: vec![provider_job_id.to_owned()],
            actual_job_conclusions: BTreeMap::from([(
                provider_job_id.to_owned(),
                "success".to_owned(),
            )]),
            ..EvidenceRecord::default()
        };
        let execution = ExecutionObservation {
            jobs: vec![JobObservation {
                job_id: provider_job_id.to_owned(),
                job_name: "scan".to_owned(),
                workload_id: "scan".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                event: "push".to_owned(),
                ..JobObservation::default()
            }],
            ..ExecutionObservation::default()
        };
        let mut findings = Vec::new();
        check_authoritative_jobs(manifest_repository, &record, &execution, &mut findings);
        assert!(g0_codes(&findings).contains("job-source-join-unverified"));
        assert!(
            !g0_codes(&findings).contains("job-inventory-mismatch"),
            "display-name equality must not masquerade as a source-job identity mismatch or join: {findings:?}"
        );
    }

    #[test]
    fn root_execution_event_must_be_declared_by_immutable_workflow_source() {
        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        let workflow_bytes =
            b"on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n";
        replace_g0_workflow_source_bytes(&mut inventory, 0, workflow_bytes);
        inventory.collector_snapshot.repositories[0].workflows[0].events = vec!["push".to_owned()];
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(
            g0_codes(&findings) == BTreeSet::from(["execution-event-source".to_owned()]),
            "consistent source/inventory hashes should produce only the root event mismatch: {findings:?}"
        );

        let execution = &snapshot.repositories[0].open_prs[0].executions[0];
        assert_eq!(execution.event, "pull_request");
        let record = EvidenceRecord {
            repository: snapshot.repositories[0].repository.clone(),
            provider: execution.provider.clone(),
            event: execution.event.clone(),
            ..Default::default()
        };
        findings.clear();
        check_authoritative_children_from_source(
            &manifest.repositories[0],
            Some(&inventory),
            &record,
            execution,
            &mut findings,
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "execution-event-source"),
            "pull_request execution must fail when immutable source declares only push: {findings:?}"
        );
    }

    #[test]
    fn event_triggers_are_root_runs_not_source_derived_child_edges() {
        for event in ["workflow_dispatch", "workflow_run"] {
            let (manifest, snapshot, mut inventory) = complete_g0_fixture();
            let workflow_bytes = format!(
                "on: [{event}]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n"
            );
            replace_g0_workflow_source_bytes(&mut inventory, 0, workflow_bytes.as_bytes());
            inventory.collector_snapshot.repositories[0].workflows[0].events =
                vec![event.to_owned()];
            refresh_typed_inventory_bytes(&mut inventory);

            let workflow = &inventory.collector_snapshot.repositories[0].workflows[0];
            let dependencies = workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
                .cloned()
                .collect::<Vec<_>>();
            let plan = derive_workflow_plan(&workflow.source, &dependencies)
                .expect("derive workflow trigger plan");
            assert!(plan.child_edges.iter().any(|edge| edge.event == event));
            assert!(plan
                .child_edges
                .iter()
                .all(|edge| !g0_child_edge_is_supported(edge)));

            let mut malformed_relation = plan.clone();
            malformed_relation.child_edges[0].relation = if event == "workflow_run" {
                "dispatch".to_owned()
            } else {
                "workflow_run".to_owned()
            };
            let mut relation_findings = Vec::new();
            check_g0_derived_plan(
                &manifest.repositories[0].repository,
                &manifest.repositories[0],
                workflow,
                &malformed_relation,
                &mut relation_findings,
            );
            assert!(relation_findings.iter().any(|finding| {
                finding.code == "g0-workflow-child"
                    && finding.message.contains("does not match event")
            }));

            let mut findings = Vec::new();
            check_g0_derived_plan(
                &manifest.repositories[0].repository,
                &manifest.repositories[0],
                workflow,
                &plan,
                &mut findings,
            );
            assert!(
                findings.is_empty(),
                "root trigger {event} must not require child-workflow contract: {findings:?}"
            );
            assert!(
                !g0_source_child_workloads(&manifest, &inventory.collector_snapshot).contains(&(
                    manifest.repositories[0].repository.clone(),
                    "scan".to_owned()
                ))
            );

            let mut execution = snapshot.repositories[0].main_executions[0].clone();
            execution.event = event.to_owned();
            let record = EvidenceRecord {
                repository: manifest.repositories[0].repository.clone(),
                provider: execution.provider.clone(),
                event: event.to_owned(),
                ..Default::default()
            };
            findings.clear();
            check_authoritative_children_from_source(
                &manifest.repositories[0],
                Some(&inventory),
                &record,
                &execution,
                &mut findings,
            );
            assert!(
                findings.is_empty(),
                "root trigger {event} with no child links must remain valid: {findings:?}"
            );

            let mut false_child_manifest = manifest.repositories[0].clone();
            false_child_manifest.expected_jobs[0].child_workflow = Some(ChildWorkflowSpec {
                repository: false_child_manifest.repository.clone(),
                workflow_path: workflow.source.path.clone(),
                event: event.to_owned(),
            });
            findings.clear();
            check_g0_derived_plan(
                &manifest.repositories[0].repository,
                &false_child_manifest,
                workflow,
                &plan,
                &mut findings,
            );
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.code == "g0-workflow-child"),
                "trigger {event} cannot satisfy a child-workflow manifest relation: {findings:?}"
            );
        }
    }

    #[test]
    fn derived_matrix_assignments_are_unique_and_expansion_stays_blocked() {
        let (manifest, _, inventory) = complete_g0_fixture();
        let repository = &manifest.repositories[0];
        let workflow = &inventory.collector_snapshot.repositories[0].workflows[0];
        let make_job = |os: &str| crate::g0_workflow::DerivedWorkflowJob {
            job_id: "scan".to_owned(),
            provider: "github".to_owned(),
            platform: "linux".to_owned(),
            architecture: "amd64".to_owned(),
            uses_reusable_workflow: false,
            matrix: BTreeMap::from([("os".to_owned(), os.to_owned())]),
        };
        let mut plan = DerivedWorkflowPlan {
            jobs: vec![make_job("ubuntu-24.04"), make_job("macos-14")],
            child_edges: Vec::new(),
            events: BTreeSet::from(["push".to_owned()]),
            has_action_steps: false,
        };
        let raw_ids = inventory
            .collector_snapshot
            .raw_objects
            .iter()
            .map(|raw| raw.raw_id.clone())
            .collect::<BTreeSet<_>>();
        let mut findings = Vec::new();
        check_g0_source_jobs(
            &repository.repository,
            Some(repository),
            workflow,
            &plan,
            &raw_ids,
            &mut findings,
        );
        assert!(findings.iter().any(|finding| {
            finding.code == "g0-source-job-matrix"
                && finding.message.contains("no concrete matrix identity")
        }));

        plan.jobs[1].matrix = plan.jobs[0].matrix.clone();
        findings.clear();
        check_g0_source_jobs(
            &repository.repository,
            Some(repository),
            workflow,
            &plan,
            &raw_ids,
            &mut findings,
        );
        assert!(findings.iter().any(|finding| {
            finding.code == "g0-source-job-matrix"
                && finding
                    .message
                    .contains("unique concrete matrix assignments")
        }));
    }

    #[test]
    fn non_manifest_workflow_events_and_source_jobs_bind_to_source_projection() {
        let (manifest, _, inventory) = complete_g0_fixture();
        let captured = &inventory.collector_snapshot.repositories[0].workflows[0];
        let dependencies = captured
            .reusable_workflows
            .iter()
            .chain(captured.actions.iter())
            .chain(captured.scanners.iter())
            .cloned()
            .collect::<Vec<_>>();
        let plan =
            derive_workflow_plan(&captured.source, &dependencies).expect("fixture source derives");
        let raw_ids = inventory
            .collector_snapshot
            .raw_objects
            .iter()
            .map(|raw| raw.raw_id.clone())
            .collect::<BTreeSet<_>>();

        let mut unreviewed_workflow = captured.clone();
        unreviewed_workflow.source_jobs[0].required = false;
        let mut workflow_derivation = WorkflowDerivationContext::default();
        let mut findings = Vec::new();
        check_g0_workflow_projection(
            &manifest.repositories[0].repository,
            None,
            &unreviewed_workflow,
            &plan,
            &raw_ids,
            &mut workflow_derivation,
            &mut findings,
        );
        assert!(findings.is_empty(), "valid source projection: {findings:?}");

        unreviewed_workflow.events = vec!["workflow_dispatch".to_owned()];
        findings.clear();
        check_g0_workflow_projection(
            &manifest.repositories[0].repository,
            None,
            &unreviewed_workflow,
            &plan,
            &raw_ids,
            &mut workflow_derivation,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-workflow-events"));

        unreviewed_workflow.events = plan.events.iter().cloned().collect();
        unreviewed_workflow.source_jobs[0].workload_id = "forged-workload".to_owned();
        unreviewed_workflow.source_jobs[0].required = true;
        findings.clear();
        check_g0_workflow_projection(
            &manifest.repositories[0].repository,
            None,
            &unreviewed_workflow,
            &plan,
            &raw_ids,
            &mut workflow_derivation,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-source-job-derivation"));
    }

    #[test]
    fn g0_main_check_joins_require_exact_runs_workflow_jobs_and_check_rows() {
        let (manifest, baseline_snapshot, baseline_inventory) = complete_g0_fixture();
        let assert_mismatch = |snapshot: &SnapshotDocument, inventory: &G0InventoryEvidence| {
            let mut findings = Vec::new();
            check_g0_inventory(&manifest, snapshot, Some(inventory), &mut findings);
            assert!(
                g0_codes(&findings).contains("g0-check-snapshot-mismatch"),
                "expected exact main-check join failure: {findings:?}"
            );
        };

        let mut wrong_path = baseline_snapshot.clone();
        wrong_path.repositories[0].main_executions[0].workflow_path =
            ".github/workflows/unreviewed.yml".to_owned();
        assert_mismatch(&wrong_path, &baseline_inventory);

        let mut wrong_revision = baseline_snapshot.clone();
        wrong_revision.repositories[0].main_executions[0].workflow_revision = sha('z');
        assert_mismatch(&wrong_revision, &baseline_inventory);

        let mut missing_job = baseline_snapshot.clone();
        missing_job.repositories[0].main_executions[0].jobs.clear();
        assert_mismatch(&missing_job, &baseline_inventory);

        let mut missing_snapshot_check = baseline_snapshot.clone();
        missing_snapshot_check.repositories[0].main_executions[0]
            .required_checks
            .clear();
        assert_mismatch(&missing_snapshot_check, &baseline_inventory);

        let mut wrong_snapshot_check_run = baseline_snapshot.clone();
        wrong_snapshot_check_run.repositories[0].main_executions[0].required_checks[0].run_id += 1;
        assert_mismatch(&wrong_snapshot_check_run, &baseline_inventory);

        let mut extra_snapshot_check = baseline_snapshot.clone();
        let mut extra =
            extra_snapshot_check.repositories[0].main_executions[0].required_checks[0].clone();
        extra.context = "unreviewed-extra".to_owned();
        extra_snapshot_check.repositories[0].main_executions[0]
            .required_checks
            .push(extra);
        assert_mismatch(&extra_snapshot_check, &baseline_inventory);

        let mut duplicate_snapshot_check = baseline_snapshot.clone();
        let duplicate =
            duplicate_snapshot_check.repositories[0].main_executions[0].required_checks[0].clone();
        duplicate_snapshot_check.repositories[0].main_executions[0]
            .required_checks
            .push(duplicate);
        assert_mismatch(&duplicate_snapshot_check, &baseline_inventory);

        let mut partial_inventory = baseline_inventory.clone();
        partial_inventory.collector_snapshot.repositories[0]
            .main_checks
            .clear();
        refresh_typed_inventory_bytes(&mut partial_inventory);
        assert_mismatch(&baseline_snapshot, &partial_inventory);
        let mut findings = Vec::new();
        check_g0_inventory(
            &manifest,
            &baseline_snapshot,
            Some(&partial_inventory),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-check-inventory"));

        let mut duplicate_producer = baseline_inventory.clone();
        let duplicate =
            duplicate_producer.collector_snapshot.repositories[0].main_checks[0].clone();
        duplicate_producer.collector_snapshot.repositories[0]
            .main_checks
            .push(duplicate);
        refresh_typed_inventory_bytes(&mut duplicate_producer);
        assert_mismatch(&baseline_snapshot, &duplicate_producer);

        let mut extra_execution = baseline_snapshot;
        let mut extra = extra_execution.repositories[0].main_executions[0].clone();
        extra.run_id += 100_000;
        extra.required_checks.clear();
        extra_execution.repositories[0].main_executions.push(extra);
        assert_mismatch(&extra_execution, &baseline_inventory);
    }

    #[test]
    fn g0_pr_check_joins_require_exact_execution_workflow_job_and_rows() {
        let (manifest, baseline_snapshot, baseline_inventory) = complete_g0_fixture();
        let assert_mismatch = |snapshot: &SnapshotDocument, inventory: &G0InventoryEvidence| {
            let mut findings = Vec::new();
            check_g0_inventory(&manifest, snapshot, Some(inventory), &mut findings);
            assert!(
                g0_codes(&findings).contains("g0-pr-check-snapshot-mismatch")
                    || g0_codes(&findings).contains("g0-pr-workflow-snapshot-mismatch")
                    || g0_codes(&findings).contains("g0-pr-workflow-binding"),
                "expected exact PR-check join failure: {findings:?}"
            );
        };

        let mut wrong_path = baseline_snapshot.clone();
        wrong_path.repositories[0].open_prs[0].executions[0].workflow_path =
            ".github/workflows/unreviewed.yml".to_owned();
        assert_mismatch(&wrong_path, &baseline_inventory);

        let mut wrong_revision = baseline_snapshot.clone();
        wrong_revision.repositories[0].open_prs[0].executions[0].workflow_revision = sha('z');
        assert_mismatch(&wrong_revision, &baseline_inventory);

        let mut missing_job = baseline_snapshot.clone();
        missing_job.repositories[0].open_prs[0].executions[0]
            .jobs
            .clear();
        assert_mismatch(&missing_job, &baseline_inventory);

        let mut missing_snapshot_check = baseline_snapshot.clone();
        missing_snapshot_check.repositories[0].open_prs[0].executions[0]
            .required_checks
            .clear();
        assert_mismatch(&missing_snapshot_check, &baseline_inventory);

        let mut wrong_snapshot_check_run = baseline_snapshot.clone();
        wrong_snapshot_check_run.repositories[0].open_prs[0].executions[0].required_checks[0]
            .run_id += 1;
        assert_mismatch(&wrong_snapshot_check_run, &baseline_inventory);

        let mut extra_snapshot_check = baseline_snapshot.clone();
        let mut extra = extra_snapshot_check.repositories[0].open_prs[0].executions[0]
            .required_checks[0]
            .clone();
        extra.context = "unreviewed-extra".to_owned();
        extra_snapshot_check.repositories[0].open_prs[0].executions[0]
            .required_checks
            .push(extra);
        assert_mismatch(&extra_snapshot_check, &baseline_inventory);

        let mut duplicate_snapshot_check = baseline_snapshot.clone();
        let duplicate = duplicate_snapshot_check.repositories[0].open_prs[0].executions[0]
            .required_checks[0]
            .clone();
        duplicate_snapshot_check.repositories[0].open_prs[0].executions[0]
            .required_checks
            .push(duplicate);
        assert_mismatch(&duplicate_snapshot_check, &baseline_inventory);

        let mut duplicate_producer = baseline_inventory.clone();
        let duplicate = duplicate_producer.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers[0]
            .clone();
        duplicate_producer.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers
            .push(duplicate);
        refresh_typed_inventory_bytes(&mut duplicate_producer);
        assert_mismatch(&baseline_snapshot, &duplicate_producer);

        let mut missing_producer = baseline_inventory.clone();
        missing_producer.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers
            .clear();
        refresh_typed_inventory_bytes(&mut missing_producer);
        assert_mismatch(&baseline_snapshot, &missing_producer);

        let mut extra_execution = baseline_snapshot;
        let mut extra = extra_execution.repositories[0].open_prs[0].executions[0].clone();
        extra.run_id += 100_000;
        extra.required_checks.clear();
        extra_execution.repositories[0].open_prs[0]
            .executions
            .push(extra);
        assert_mismatch(&extra_execution, &baseline_inventory);
    }

    #[test]
    fn child_run_ids_are_repository_scoped_and_reusable_urls_use_caller_repo() {
        let root_repository = "owner/caller";
        let root = ExecutionObservation {
            run_id: 7,
            run_attempt: 2,
            workflow_path: ".github/workflows/caller.yml".to_owned(),
            workflow_revision: sha('a'),
            ..Default::default()
        };
        let direct = ChildWorkflowObservation {
            parent_run_id: 7,
            parent_run_attempt: 2,
            parent_repository: root_repository.to_owned(),
            parent_workflow_path: ".github/workflows/caller.yml".to_owned(),
            parent_source_sha: sha('a'),
            run_id: 7,
            run_attempt: 2,
            repository: "owner/reusable".to_owned(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('b'),
            source_url: "https://github.com/owner/caller/actions/runs/7".to_owned(),
            ..Default::default()
        };
        let nested = ChildWorkflowObservation {
            parent_run_id: 7,
            parent_run_attempt: 2,
            parent_repository: "owner/reusable".to_owned(),
            parent_workflow_path: ".github/workflows/reusable.yml".to_owned(),
            parent_source_sha: sha('b'),
            run_id: 7,
            run_attempt: 2,
            repository: "owner/nested".to_owned(),
            workflow_path: ".github/workflows/nested.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('c'),
            source_url: "https://github.com/owner/caller/actions/runs/7".to_owned(),
            ..Default::default()
        };
        let mut root_with_nested = root.clone();
        root_with_nested.child_workflows = vec![direct.clone(), nested.clone()];
        assert!(g0_child_workflow_run_identity_is_valid(
            &direct,
            root_repository,
            &root_with_nested
        ));
        assert!(g0_child_workflow_run_url_is_valid(
            &direct,
            root_repository,
            &root_with_nested
        ));
        assert!(g0_child_workflow_run_identity_is_valid(
            &nested,
            root_repository,
            &root_with_nested
        ));
        assert!(g0_child_workflow_run_url_is_valid(
            &nested,
            root_repository,
            &root_with_nested
        ));

        let mut wrong_nested_url = nested.clone();
        wrong_nested_url.source_url = "https://github.com/owner/nested/actions/runs/7".to_owned();
        assert!(!g0_child_workflow_run_url_is_valid(
            &wrong_nested_url,
            root_repository,
            &root_with_nested
        ));

        let separate_parent = ChildWorkflowObservation {
            parent_run_id: 7,
            parent_run_attempt: 2,
            parent_repository: root_repository.to_owned(),
            parent_workflow_path: ".github/workflows/caller.yml".to_owned(),
            parent_source_sha: sha('a'),
            run_id: 8,
            run_attempt: 1,
            repository: "owner/reusable".to_owned(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_dispatch".to_owned(),
            source_sha: sha('d'),
            source_url: "https://github.com/owner/reusable/actions/runs/8".to_owned(),
            ..Default::default()
        };
        let nested_in_separate_run = ChildWorkflowObservation {
            parent_run_id: 8,
            parent_run_attempt: 1,
            parent_repository: "owner/reusable".to_owned(),
            parent_workflow_path: ".github/workflows/reusable.yml".to_owned(),
            parent_source_sha: sha('d'),
            run_id: 8,
            run_attempt: 1,
            repository: "owner/nested".to_owned(),
            workflow_path: ".github/workflows/nested.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('e'),
            source_url: "https://github.com/owner/reusable/actions/runs/8".to_owned(),
            ..Default::default()
        };
        let mut root_with_separate_run = root;
        root_with_separate_run.child_workflows =
            vec![separate_parent.clone(), nested_in_separate_run.clone()];
        assert!(
            g0_child_workflow_run_identity_is_valid(
                &nested_in_separate_run,
                root_repository,
                &root_with_separate_run
            ),
            "nested reusable workflow must bind to its separate caller run"
        );
        assert!(g0_child_workflow_run_url_is_valid(
            &nested_in_separate_run,
            root_repository,
            &root_with_separate_run
        ));

        let mut same_repository_collision = ChildWorkflowObservation {
            parent_run_id: 7,
            parent_run_attempt: 2,
            parent_repository: root_repository.to_owned(),
            parent_workflow_path: ".github/workflows/caller.yml".to_owned(),
            parent_source_sha: sha('a'),
            run_id: 7,
            run_attempt: 2,
            repository: "OWNER/CALLER".to_owned(),
            workflow_path: ".github/workflows/child.yml".to_owned(),
            event: "workflow_dispatch".to_owned(),
            source_sha: sha('f'),
            source_url: "https://github.com/OWNER/CALLER/actions/runs/7".to_owned(),
            ..Default::default()
        };
        assert!(
            !g0_child_workflow_run_identity_is_valid(
                &same_repository_collision,
                root_repository,
                &root_with_nested
            ),
            "repository identity comparison must be case-insensitive"
        );
        same_repository_collision.run_id += 1;
        same_repository_collision.source_url =
            "https://github.com/OWNER/CALLER/actions/runs/8".to_owned();
        assert!(g0_child_workflow_run_identity_is_valid(
            &same_repository_collision,
            root_repository,
            &root_with_nested
        ));
    }

    #[test]
    fn release_producer_membership_requires_exact_repository_workflow_source_and_url() {
        let record = EvidenceRecord {
            repository: "owner/caller".to_owned(),
            workflow_path: ".github/workflows/release.yml".to_owned(),
            run_id: 7,
            run_url: "https://github.com/owner/caller/actions/runs/7".to_owned(),
            actual_checkout_sha: sha('a'),
            ..Default::default()
        };
        let root_producer = ReleaseEvidenceProducer {
            repository: "owner/caller".to_owned(),
            workflow_path: ".github/workflows/release.yml".to_owned(),
            run_id: 7,
            run_url: "https://github.com/owner/caller/actions/runs/7".to_owned(),
            source_commit: sha('a'),
            manifest_sha256: digest('a'),
        };
        assert!(g0_release_producer_is_record_run(&root_producer, &record));
        let mut foreign_root_producer = root_producer.clone();
        foreign_root_producer.repository = "owner/other".to_owned();
        assert!(!g0_release_producer_is_record_run(
            &foreign_root_producer,
            &record
        ));

        let child_link = ChildWorkflowLink {
            parent_run_id: 7,
            parent_run_attempt: 2,
            parent_repository: "owner/caller".to_owned(),
            parent_workflow_path: ".github/workflows/release.yml".to_owned(),
            parent_source_sha: sha('a'),
            run_id: 8,
            run_attempt: 1,
            repository: "owner/child".to_owned(),
            workflow_path: ".github/workflows/child-release.yml".to_owned(),
            event: "workflow_dispatch".to_owned(),
            source_sha: sha('b'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            run_url: "https://github.com/owner/child/actions/runs/8".to_owned(),
        };
        let child_producer = ReleaseEvidenceProducer {
            repository: "owner/child".to_owned(),
            workflow_path: ".github/workflows/child-release.yml".to_owned(),
            run_id: 8,
            run_url: "https://github.com/owner/child/actions/runs/8".to_owned(),
            source_commit: sha('b'),
            manifest_sha256: digest('b'),
        };
        assert!(g0_release_producer_matches_child_link(
            &child_producer,
            &child_link
        ));
        let mut wrong_repository = child_producer.clone();
        wrong_repository.repository = "owner/foreign".to_owned();
        let mut wrong_workflow = child_producer.clone();
        wrong_workflow.workflow_path = ".github/workflows/other.yml".to_owned();
        let mut wrong_url = child_producer.clone();
        wrong_url.run_url = "https://github.com/owner/child/actions/runs/9".to_owned();
        let mut wrong_source = child_producer;
        wrong_source.source_commit = sha('c');
        for (field, foreign) in [
            ("repository", wrong_repository),
            ("workflow", wrong_workflow),
            ("URL", wrong_url),
            ("source", wrong_source),
        ] {
            assert!(
                !g0_release_producer_matches_child_link(&foreign, &child_link),
                "producer with mismatched {field} matched a child link"
            );
        }
    }

    #[test]
    fn snapshot_execution_identity_is_unique_across_main_and_pr_lanes() {
        let (manifest, mut snapshot, _) = complete_g0_fixture();
        snapshot.repositories[0].open_prs[0].executions[0].run_id =
            snapshot.repositories[0].main_executions[0].run_id;
        let mut findings = Vec::new();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(g0_codes(&findings).contains("snapshot-duplicate-execution"));

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        let parent_repository = snapshot.repositories[0].repository.clone();
        let child_repository = snapshot.repositories[1].repository.clone();
        let parent = snapshot.repositories[0].main_executions[0].clone();
        let other_repository_run_id = parent.run_id;
        let other_repository_attempt = parent.run_attempt;
        snapshot.repositories[0].main_executions[0]
            .child_workflows
            .push(ChildWorkflowObservation {
                parent_run_id: parent.run_id,
                parent_run_attempt: parent.run_attempt,
                parent_repository: parent_repository.clone(),
                parent_workflow_path: parent.workflow_path.clone(),
                parent_source_sha: parent.workflow_revision.clone(),
                run_id: other_repository_run_id,
                run_attempt: other_repository_attempt,
                repository: child_repository.clone(),
                workflow_path: ".github/workflows/child.yml".to_owned(),
                event: "workflow_dispatch".to_owned(),
                source_sha: sha('b'),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                source_url: format!(
                    "https://github.com/{child_repository}/actions/runs/{other_repository_run_id}"
                ),
                ..Default::default()
            });
        findings.clear();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(
            !g0_codes(&findings).contains("snapshot-duplicate-execution"),
            "equal run IDs in different repositories were treated as a collision"
        );
        assert!(
            !g0_codes(&findings).contains("child-run-conclusion"),
            "a cross-repository child was required to have a different numeric run ID"
        );

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        let parent_repository = snapshot.repositories[0].repository.clone();
        let child_repository = snapshot.repositories[1].repository.clone();
        let parent = snapshot.repositories[0].main_executions[0].clone();
        let child_run = snapshot.repositories[1].main_executions[0].clone();
        snapshot.repositories[0].main_executions[0]
            .child_workflows
            .push(ChildWorkflowObservation {
                parent_run_id: parent.run_id,
                parent_run_attempt: parent.run_attempt,
                parent_repository: parent_repository.clone(),
                parent_workflow_path: parent.workflow_path.clone(),
                parent_source_sha: parent.workflow_revision.clone(),
                run_id: child_run.run_id,
                run_attempt: child_run.run_attempt,
                repository: child_repository.to_ascii_uppercase(),
                workflow_path: ".github/workflows/child.yml".to_owned(),
                event: "workflow_dispatch".to_owned(),
                source_sha: sha('b'),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                source_url: format!(
                    "https://github.com/{}/actions/runs/{}",
                    child_repository.to_ascii_uppercase(),
                    child_run.run_id
                ),
                ..Default::default()
            });
        findings.clear();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(
            g0_codes(&findings).contains("snapshot-duplicate-execution"),
            "a child run was not compared with executions in its own repository"
        );

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        snapshot.repositories[0].open_prs[0].executions[0].run_id =
            snapshot.repositories[0].main_executions[0].run_id;
        snapshot.repositories[0].open_prs[0].executions[0].run_attempt += 1;
        findings.clear();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(!g0_codes(&findings).contains("snapshot-duplicate-execution"));

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        let repo = &mut snapshot.repositories[0];
        let repository = repo.repository.clone();
        let main = repo.main_executions[0].clone();
        let pr_execution = &mut repo.open_prs[0].executions[0];
        let pr_workflow_path = pr_execution.workflow_path.clone();
        let pr_workflow_revision = pr_execution.workflow_revision.clone();
        pr_execution.child_workflows.push(ChildWorkflowObservation {
            parent_run_id: pr_execution.run_id,
            parent_run_attempt: pr_execution.run_attempt,
            parent_repository: repository.clone(),
            parent_workflow_path: pr_workflow_path,
            parent_source_sha: pr_workflow_revision,
            run_id: main.run_id,
            run_attempt: main.run_attempt,
            repository: repository.clone(),
            workflow_path: ".github/workflows/child.yml".to_owned(),
            event: "workflow_dispatch".to_owned(),
            source_sha: sha('b'),
            ..Default::default()
        });
        findings.clear();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(g0_codes(&findings).contains("snapshot-duplicate-execution"));

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        let repo = &mut snapshot.repositories[0];
        let repository = repo.repository.clone();
        let main = &mut repo.main_executions[0];
        let run_id = main.run_id;
        let run_attempt = main.run_attempt;
        main.child_workflows.push(ChildWorkflowObservation {
            parent_run_id: run_id,
            parent_run_attempt: run_attempt,
            parent_repository: repository.clone(),
            parent_workflow_path: main.workflow_path.clone(),
            parent_source_sha: main.workflow_revision.clone(),
            run_id,
            run_attempt,
            repository: repository.clone(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('b'),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            source_url: format!("https://github.com/{repository}/actions/runs/{run_id}"),
            ..Default::default()
        });
        findings.clear();
        check_snapshot(&manifest, &snapshot, &mut findings);
        assert!(!g0_codes(&findings).contains("snapshot-duplicate-execution"));
    }

    #[test]
    fn github_endpoint_and_read_scope_policy_fail_closed() {
        assert!(g0_api_base("https://api.github.com"));
        assert!(g0_api_base("https://api.github.com/"));
        for unsupported in [
            "http://api.github.com",
            "https://api.github.com.evil.example",
            "https://user:pass@api.github.com",
            "https://api.github.com:8443",
            "https://api.github.com/api/v3",
            "https://api.github.com?enterprise=true",
            "https://github.enterprise.example/api/v3",
        ] {
            assert!(!g0_api_base(unsupported), "accepted {unsupported}");
        }

        let supported = G0_ALLOWED_SCOPES
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>();
        assert!(g0_scope_list_is_allowed(&supported));
        assert!(!g0_scope_list_is_allowed(&[]));
        for unsupported in [
            vec!["metadata:read".to_owned(), "metadata:read".to_owned()],
            vec!["actions:write".to_owned()],
            vec!["workflows:read".to_owned()],
        ] {
            assert!(!g0_scope_list_is_allowed(&unsupported));
        }

        let (manifest, mut snapshot, _) = complete_g0_fixture();
        snapshot.source.api_base = "https://api.github.com.evil.example".to_owned();
        snapshot.source.permission_scopes = vec!["actions:write".to_owned()];
        let mut findings = Vec::new();
        check_snapshot(&manifest, &snapshot, &mut findings);
        let codes = g0_codes(&findings);
        assert!(codes.contains("snapshot-source"));
        assert!(codes.contains("snapshot-access"));
    }

    #[test]
    fn graphql_requests_without_authorized_query_variable_pairs_are_rejected() {
        let (_, _, inventory) = complete_g0_fixture();
        let mut graphql = inventory.collector_snapshot.requests[0].clone();
        graphql.api = G0ApiKind::Graphql;
        graphql.endpoint_or_operation = "repository".to_owned();
        let query = b"query { repository(name: \"velnor\") { name } }";
        let variables = br#"{"owner":"tailrocks","name":"velnor"}"#;
        assert!(!g0_request_semantics(
            &graphql,
            Some(query),
            Some(variables)
        ));
    }

    #[test]
    fn request_base64_fields_are_bounded_before_decode_and_graphql_json_parse() {
        let oversized_bytes = vec![b'x'; G0_MAX_REQUEST_FIELD_BYTES + 1];
        let oversized_base64 = BASE64.encode(oversized_bytes);
        assert_eq!(oversized_base64.len(), G0_MAX_REQUEST_FIELD_BASE64_BYTES);
        assert!(decode_g0_request_field(&oversized_base64).is_none());

        let (_, _, inventory) = complete_g0_fixture();
        let mut collector = inventory.collector_snapshot;
        collector.requests[0].api = G0ApiKind::Graphql;
        collector.requests[0].endpoint_or_operation = "repository".to_owned();
        collector.requests[0].variables_base64 = oversized_base64.clone();
        let mut findings = Vec::new();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(g0_codes(&findings).contains("g0-request-incomplete"));

        let (manifest, snapshot, inventory) = complete_g0_fixture();
        for field in ["query", "variables"] {
            let mut oversized_inventory = inventory.clone();
            let request = &mut oversized_inventory.collector_snapshot.requests[0];
            if field == "query" {
                request.query_base64 = oversized_base64.clone();
            } else {
                request.variables_base64 = oversized_base64.clone();
            }
            let mut findings = Vec::new();
            check_g0_inventory(
                &manifest,
                &snapshot,
                Some(&oversized_inventory),
                &mut findings,
            );
            assert!(
                g0_codes(&findings).contains("g0-request-size"),
                "oversized {field} field was not rejected before snapshot serialization"
            );
        }
    }

    #[test]
    fn raw_object_bodies_are_bounded_before_decode_and_snapshot_serialization() {
        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        let oversized_bytes = vec![b'x'; G0_MAX_RAW_OBJECT_BYTES + 1];
        let oversized_base64 = BASE64.encode(oversized_bytes);
        assert_eq!(oversized_base64.len(), G0_MAX_RAW_OBJECT_BASE64_BYTES);

        let raw = &mut inventory.collector_snapshot.raw_objects[0];
        raw.bytes_base64 = oversized_base64.clone();
        raw.byte_length = (G0_MAX_RAW_OBJECT_BYTES + 1) as u64;
        assert!(!g0_raw_object_bytes_within_limit(raw));
        assert!(decode_g0_raw_object_bytes(raw).is_none());

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-raw-object-size"),
            "oversized raw body was not rejected before snapshot serialization"
        );

        let raw = &mut inventory.collector_snapshot.raw_objects[0];
        raw.bytes_base64 = "A".repeat(G0_MAX_RAW_OBJECT_BASE64_BYTES + 4);
        raw.byte_length = 2;
        assert!(!g0_raw_object_bytes_within_limit(raw));
        assert!(decode_g0_raw_object_bytes(raw).is_none());
    }

    #[test]
    fn per_repository_access_scopes_match_exact_repository_request_purpose() {
        let (_, _, inventory) = complete_g0_fixture();
        let base = inventory.collector_snapshot;
        let repository = &base.repositories[0].repository;
        let mut rules_request = base.requests[0].clone();
        rules_request.endpoint_or_operation = format!("/repos/{repository}/rules/branches/main");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["metadata:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/rulesets");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["metadata:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/rulesets/42");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["metadata:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/rulesets/rule-suites");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["administration:read"]))
        );
        let mut classic_protection_request = rules_request.clone();
        classic_protection_request.endpoint_or_operation =
            format!("/repos/{repository}/branches/main/protection/required_status_checks");
        assert_eq!(
            g0_repository_request_scopes(&classic_protection_request, repository),
            Some(BTreeSet::from(["administration:read"]))
        );
        for endpoint in [
            format!("/repos/{repository}/actions/runners"),
            format!("/repos/{repository}/actions/runners/12"),
            format!("/repos/{repository}/actions/permissions"),
            format!("/repos/{repository}/actions/permissions/workflow"),
        ] {
            rules_request.endpoint_or_operation = endpoint;
            assert_eq!(
                g0_repository_request_scopes(&rules_request, repository),
                Some(BTreeSet::from(["administration:read"]))
            );
        }
        rules_request.endpoint_or_operation =
            format!("/repos/{repository}/actions/runs/42/artifacts");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["actions:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/pulls");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["pull_requests:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/pulls/7");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["contents:read", "pull_requests:read"]))
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/pulls/7/files");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            Some(BTreeSet::from(["pull_requests:read"]))
        );
        rules_request.endpoint_or_operation =
            format!("/repos/{repository}/rules/branches/main/unrecognized");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            None
        );
        rules_request.endpoint_or_operation = format!("/repos/{repository}/secret-scanning/alerts");
        assert_eq!(
            g0_repository_request_scopes(&rules_request, repository),
            None
        );
        assert!(!g0_request_semantics(
            &rules_request,
            Some(b""),
            Some(b"{}")
        ));

        let mut findings = Vec::new();
        check_g0_access(&base, &mut findings);
        assert!(findings.is_empty(), "fixture access rejected: {findings:?}");

        let mut runner_access = base.clone();
        runner_access.requests[0].endpoint_or_operation =
            format!("/repos/{repository}/actions/runners");
        runner_access.access[0].scopes = vec![
            "actions:read".to_owned(),
            "administration:read".to_owned(),
            "contents:read".to_owned(),
        ];
        findings.clear();
        check_g0_access(&runner_access, &mut findings);
        assert!(
            findings.is_empty(),
            "runner access purpose rejected: {findings:?}"
        );

        let mut wrong_runner_scope = runner_access.clone();
        wrong_runner_scope.access[0].scopes =
            vec!["actions:read".to_owned(), "contents:read".to_owned()];
        findings.clear();
        check_g0_access(&wrong_runner_scope, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access-scope-purpose"));

        let mut unreferenced_runner_request = base.clone();
        let mut extra_request = unreferenced_runner_request.requests[0].clone();
        extra_request.request_id = "request-unreferenced-runner".to_owned();
        extra_request.endpoint_or_operation = format!("/repos/{repository}/actions/runners");
        extra_request.query_base64 = BASE64.encode(b"page=1&per_page=100");
        extra_request.query_sha256 = digest_bytes(b"page=1&per_page=100");
        extra_request.page.per_page = Some(100);
        extra_request.response_raw_ref = "raw-unreferenced-runner".to_owned();
        let mut extra_raw = unreferenced_runner_request.raw_objects[0].clone();
        extra_raw.raw_id = extra_request.response_raw_ref.clone();
        extra_raw.request_id = extra_request.request_id.clone();
        unreferenced_runner_request.requests.push(extra_request);
        unreferenced_runner_request.raw_objects.push(extra_raw);

        let mut request_findings = Vec::new();
        check_g0_request_provenance(&unreferenced_runner_request, &mut request_findings);
        assert!(
            !g0_codes(&request_findings).contains("g0-request-incomplete"),
            "valid runner request was rejected before scope binding: {request_findings:?}"
        );
        findings.clear();
        check_g0_access(&unreferenced_runner_request, &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-access-scope-purpose"),
            "unreferenced runner response bypassed its required per-repository scope"
        );

        let mut pull_detail_access = base.clone();
        pull_detail_access.requests[0].endpoint_or_operation =
            format!("/repos/{repository}/pulls/7");
        pull_detail_access.access[0].scopes =
            vec!["actions:read".to_owned(), "contents:read".to_owned()];
        findings.clear();
        check_g0_access(&pull_detail_access, &mut findings);
        assert!(
            findings.is_empty(),
            "PR detail contents permission was rejected: {findings:?}"
        );

        let mut wrong_purpose = base.clone();
        wrong_purpose.access[0].scopes = vec!["contents:read".to_owned()];
        findings.clear();
        check_g0_access(&wrong_purpose, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access-scope-purpose"));

        let mut duplicate_scope = base.clone();
        duplicate_scope.access[0].scopes =
            vec!["metadata:read".to_owned(), "metadata:read".to_owned()];
        findings.clear();
        check_g0_access(&duplicate_scope, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access"));

        let mut unknown_scope = base.clone();
        unknown_scope.access[0].scopes = vec!["workflows:read".to_owned()];
        findings.clear();
        check_g0_access(&unknown_scope, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access"));
        assert!(g0_codes(&findings).contains("g0-access-scope-purpose"));

        let mut write_scope = base.clone();
        write_scope.access[0].scopes = vec!["metadata:write".to_owned()];
        findings.clear();
        check_g0_access(&write_scope, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access"));

        let mut cross_repository_ref = base;
        cross_repository_ref.access[1].raw_object_refs = vec!["raw-repository-1".to_owned()];
        findings.clear();
        check_g0_access(&cross_repository_ref, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access-scope-purpose"));

        let mut duplicate_raw_ref = cross_repository_ref;
        duplicate_raw_ref.access[0]
            .raw_object_refs
            .push("raw-repository-1".to_owned());
        findings.clear();
        check_g0_access(&duplicate_raw_ref, &mut findings);
        assert!(g0_codes(&findings).contains("g0-access-scope-purpose"));
    }

    #[test]
    fn rest_query_parameters_are_bound_to_endpoint_purpose() {
        let (_, _, inventory) = complete_g0_fixture();
        let repository = &inventory.collector_snapshot.repositories[0].repository;
        let mut pulls_request = inventory.collector_snapshot.requests[1].clone();
        pulls_request.endpoint_or_operation = format!("/repos/{repository}/pulls");

        let open_pulls = b"page=1&per_page=100&state=open";
        pulls_request.query_base64 = BASE64.encode(open_pulls);
        pulls_request.query_sha256 = digest_bytes(open_pulls);
        assert!(g0_request_semantics(
            &pulls_request,
            Some(open_pulls),
            Some(b"{}")
        ));

        for disallowed in [
            b"page=1&per_page=100&state=closed".as_slice(),
            b"page=1&per_page=100&state=all".as_slice(),
            b"branch=main&page=1&per_page=100".as_slice(),
        ] {
            assert!(
                !g0_request_semantics(&pulls_request, Some(disallowed), Some(b"{}")),
                "unsupported PR-list query passed: {}",
                String::from_utf8_lossy(disallowed)
            );
        }

        let mut contents_request = inventory.collector_snapshot.requests[2].clone();
        contents_request.endpoint_or_operation =
            format!("/repos/{repository}/contents/.github/workflows/ci.yml");
        contents_request.page.per_page = None;
        let source_ref = format!("ref={}", sha('a'));
        assert!(g0_request_semantics(
            &contents_request,
            Some(source_ref.as_bytes()),
            Some(b"{}")
        ));
        assert!(!g0_request_semantics(
            &contents_request,
            Some(b"ref=main"),
            Some(b"{}")
        ));
    }

    #[test]
    fn rest_pagination_rejects_cursor_only_continuation() {
        let (_, _, inventory) = complete_g0_fixture();
        let mut first = inventory.collector_snapshot.requests[1].clone();
        first.page.has_next_page = true;
        first.page.cursor_out = Some("cursor-2".to_owned());
        first.page.link_next = None;
        assert!(!g0_request_semantics(
            &first,
            Some(b"page=1&per_page=100"),
            Some(b"{}")
        ));

        let mut next = first.clone();
        next.page.number = 2;
        next.page.has_next_page = false;
        next.page.cursor_in = Some("cursor-2".to_owned());
        next.page.cursor_out = None;
        assert!(!g0_next_link_matches(&first, &next));

        let mut collector = inventory.collector_snapshot;
        collector.requests[1] = first;
        let mut findings = Vec::new();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(g0_codes(&findings).contains("g0-request-incomplete"));
        assert!(g0_codes(&findings).contains("g0-pagination"));
    }

    #[test]
    fn encoded_path_components_are_canonical_and_double_dot_names_survive() {
        let (_, _, inventory) = complete_g0_fixture();
        let repository = &inventory.collector_snapshot.repositories[0];
        let mut request = inventory.collector_snapshot.requests[0].clone();
        let query = format!("ref={}", repository.default_branch_sha);
        request.endpoint_or_operation = format!(
            "/repos/{}/contents/.github/workflows/ci..yml",
            repository.repository
        );
        assert!(g0_request_semantics(
            &request,
            Some(query.as_bytes()),
            Some(b"{}")
        ));

        request.endpoint_or_operation = format!(
            "/repos/{}/contents/.github/workflows/x%2F%2E%2E%2Fci.yml",
            repository.repository
        );
        assert!(!g0_request_semantics(
            &request,
            Some(query.as_bytes()),
            Some(b"{}")
        ));

        let mut source = inventory.collector_snapshot.repositories[0].workflows[0]
            .source
            .clone();
        source.path = ".github/workflows/x%2F%2E%2E%2Fci.yml".to_owned();
        assert!(g0_workflow_file_path(&source.path));
        let encoded_endpoint = g0_repo_contents_endpoint(&source.repository, &source.path)
            .expect("literal percent filename has a canonical API path");
        assert!(encoded_endpoint.contains("x%252F%252E%252E%252Fci.yml"));
        assert!(g0_rest_endpoint_path_is_safe(&encoded_endpoint));
        assert!(g0_github_blob_path(&source).is_some_and(|path| {
            path.ends_with("/.github/workflows/x%252F%252E%252E%252Fci.yml")
        }));
        source.source_url = format!(
            "https://github.com/{}/blob/{}/{}",
            source.repository, source.source_sha, source.path
        );
        assert!(!workflow_source_url_matches(&source));
        source.source_url = format!(
            "https://github.com{}",
            g0_github_blob_path(&source).unwrap()
        );
        assert!(workflow_source_url_matches(&source));
    }

    #[test]
    fn execution_and_claimed_required_checks_reject_duplicates_and_app_conflicts() {
        let mut execution = minimal_execution();
        let check = CheckObservation {
            context: "ci".to_owned(),
            app_id: "123".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            run_id: execution.run_id,
            job_id: "1".to_owned(),
            source_url: "https://github.com/o/r/runs/1".to_owned(),
            event: execution.event.clone(),
        };
        execution.required_checks = vec![check.clone(), check.clone()];
        let mut findings = Vec::new();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-duplicate"));

        let mut conflict = check.clone();
        conflict.app_id = "456".to_owned();
        execution.required_checks = vec![check.clone(), conflict.clone()];
        findings.clear();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-duplicate"));

        let contexts = vec![
            RequiredContext {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
            },
            RequiredContext {
                context: "ci".to_owned(),
                app_id: "456".to_owned(),
            },
        ];
        findings.clear();
        check_contexts("owner/repo", &contexts, &mut findings);
        assert!(g0_codes(&findings).contains("check-context"));

        let snapshot = SnapshotRepository {
            repository: "owner/repo".to_owned(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: sha('a'),
            ruleset: RulesetObservation {
                required_checks: contexts,
                source_url: "https://github.com/owner/repo/settings/rules".to_owned(),
                pages_complete: true,
            },
            workflows: Vec::new(),
            main_executions: Vec::new(),
            open_prs: Vec::new(),
        };
        let record = EvidenceRecord {
            repository: snapshot.repository.clone(),
            required_checks: vec![
                RequiredCheckEvidence {
                    context: "ci".to_owned(),
                    app_id: "123".to_owned(),
                    job_id: "1".to_owned(),
                    status: "completed".to_owned(),
                    conclusion: "success".to_owned(),
                    run_id: 1,
                    source_url: "https://github.com/o/r/runs/1".to_owned(),
                    event: "push".to_owned(),
                },
                RequiredCheckEvidence {
                    context: "ci".to_owned(),
                    app_id: "456".to_owned(),
                    job_id: "2".to_owned(),
                    status: "completed".to_owned(),
                    conclusion: "success".to_owned(),
                    run_id: 1,
                    source_url: "https://github.com/o/r/runs/1".to_owned(),
                    event: "push".to_owned(),
                },
            ],
            ..Default::default()
        };
        execution.required_checks = vec![check, conflict];
        execution.jobs = vec![
            JobObservation {
                job_id: "1".to_owned(),
                ..Default::default()
            },
            JobObservation {
                job_id: "2".to_owned(),
                ..Default::default()
            },
        ];
        findings.clear();
        check_authoritative_checks(&snapshot, &record, &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-context-mismatch"));
    }

    #[test]
    fn required_check_claims_join_execution_rows_field_for_field() {
        let contexts = vec![
            RequiredContext {
                context: "build".to_owned(),
                app_id: "123".to_owned(),
            },
            RequiredContext {
                context: "test".to_owned(),
                app_id: "456".to_owned(),
            },
        ];
        let snapshot = SnapshotRepository {
            repository: "owner/repo".to_owned(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: sha('a'),
            ruleset: RulesetObservation {
                required_checks: contexts,
                source_url: "https://github.com/owner/repo/settings/rules".to_owned(),
                pages_complete: true,
            },
            workflows: Vec::new(),
            main_executions: Vec::new(),
            open_prs: Vec::new(),
        };
        let build_check = CheckObservation {
            context: "build".to_owned(),
            app_id: "123".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            run_id: 1,
            job_id: "job-build".to_owned(),
            source_url: "https://github.com/owner/repo/runs/101".to_owned(),
            event: "push".to_owned(),
        };
        let test_check = CheckObservation {
            context: "test".to_owned(),
            app_id: "456".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            run_id: 1,
            job_id: "job-test".to_owned(),
            source_url: "https://github.com/owner/repo/runs/102".to_owned(),
            event: "push".to_owned(),
        };
        let mut execution = minimal_execution();
        execution.required_checks = vec![build_check.clone(), test_check.clone()];
        execution.jobs = vec![
            JobObservation {
                job_id: "job-build".to_owned(),
                ..Default::default()
            },
            JobObservation {
                job_id: "job-test".to_owned(),
                ..Default::default()
            },
        ];
        let mut record = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            required_checks: vec![
                RequiredCheckEvidence {
                    context: build_check.context.clone(),
                    app_id: build_check.app_id.clone(),
                    job_id: build_check.job_id.clone(),
                    status: build_check.status.clone(),
                    conclusion: build_check.conclusion.clone(),
                    run_id: build_check.run_id,
                    source_url: build_check.source_url.clone(),
                    event: build_check.event.clone(),
                },
                RequiredCheckEvidence {
                    context: test_check.context.clone(),
                    app_id: test_check.app_id.clone(),
                    job_id: test_check.job_id.clone(),
                    status: test_check.status.clone(),
                    conclusion: test_check.conclusion.clone(),
                    run_id: test_check.run_id,
                    source_url: test_check.source_url.clone(),
                    event: test_check.event.clone(),
                },
            ],
            ..Default::default()
        };
        let mut findings = Vec::new();
        check_authoritative_checks(&snapshot, &record, &execution, &mut findings);
        assert!(
            findings.is_empty(),
            "matching check rows rejected: {findings:?}"
        );

        record.required_checks[0].job_id = "job-test".to_owned();
        record.required_checks[1].job_id = "job-build".to_owned();
        record.required_checks[0].source_url = "https://foreign.example/check/1".to_owned();
        record.required_checks[1].source_url = "https://foreign.example/check/2".to_owned();
        findings.clear();
        check_authoritative_checks(&snapshot, &record, &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-observation-mismatch"));

        record.required_checks[0].job_id = build_check.job_id.clone();
        record.required_checks[1].job_id = test_check.job_id.clone();
        record.required_checks[0].source_url = build_check.source_url.clone();
        record.required_checks[1].source_url = test_check.source_url.clone();
        execution.required_checks[0].source_url = "https://foreign.example/check/3".to_owned();
        findings.clear();
        check_authoritative_checks(&snapshot, &record, &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-observation-mismatch"));

        let mut extra = test_check;
        extra.context = "unreviewed".to_owned();
        extra.app_id = "789".to_owned();
        execution.required_checks.push(extra);
        findings.clear();
        check_authoritative_checks(&snapshot, &record, &execution, &mut findings);
        assert!(g0_codes(&findings).contains("check-observation-mismatch"));
    }

    #[test]
    fn g0_workflow_source_requires_pinned_contents_request_and_response() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let workflow_raw_id = inventory.collector_snapshot.repositories[0].workflows[0]
            .source
            .raw_object_refs[0]
            .clone();
        let workflow_request = inventory
            .collector_snapshot
            .requests
            .iter()
            .find(|request| request.response_raw_ref == workflow_raw_id)
            .expect("fixture has exact workflow response request");
        assert!(workflow_request
            .endpoint_or_operation
            .ends_with("/contents/.github/workflows/ci.yml"));
        assert_eq!(workflow_request.accept, "application/vnd.github.raw+json");

        let mut wrong_endpoint = inventory.clone();
        let workflow_request = wrong_endpoint
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.response_raw_ref == workflow_raw_id)
            .expect("workflow response request");
        workflow_request.endpoint_or_operation = "/repos/tailrocks/velnor".to_owned();
        refresh_typed_inventory_bytes(&mut wrong_endpoint);
        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_endpoint), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));

        let mut wrong_response = inventory;
        let workflow_request = wrong_response
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.response_raw_ref == workflow_raw_id)
            .expect("workflow response request");
        workflow_request.response_raw_ref = "raw-repository-1".to_owned();
        refresh_typed_inventory_bytes(&mut wrong_response);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_response), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));
    }

    #[test]
    fn g0_workflow_source_requires_exact_pinned_ref_and_raw_accept() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let workflow_raw_id = inventory.collector_snapshot.repositories[0].workflows[0]
            .source
            .raw_object_refs[0]
            .clone();
        let mut findings = Vec::new();

        let mut wrong_ref = inventory.clone();
        let workflow_request = wrong_ref
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.response_raw_ref == workflow_raw_id)
            .expect("workflow response request");
        let wrong_query = format!("ref={}", sha('z'));
        workflow_request.query_base64 = BASE64.encode(wrong_query.as_bytes());
        workflow_request.query_sha256 = digest_bytes(wrong_query.as_bytes());
        refresh_typed_inventory_bytes(&mut wrong_ref);
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_ref), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));

        let mut wrong_accept = inventory;
        let workflow_request = wrong_accept
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.response_raw_ref == workflow_raw_id)
            .expect("workflow response request");
        workflow_request.accept = "application/vnd.github+json".to_owned();
        refresh_typed_inventory_bytes(&mut wrong_accept);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_accept), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));
    }

    #[test]
    fn g0_root_workflow_path_must_be_a_direct_workflows_file() {
        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        let nested_path = ".github/workflows/nested/ci.yml";
        let (repository, raw_id) = {
            let source = &mut inventory.collector_snapshot.repositories[0].workflows[0].source;
            source.path = nested_path.to_owned();
            source.source_url = format!(
                "https://github.com/{}/blob/{}/{nested_path}",
                source.repository, source.source_sha
            );
            (source.repository.clone(), source.raw_object_refs[0].clone())
        };
        let request = inventory
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.response_raw_ref == raw_id)
            .expect("workflow response request");
        request.endpoint_or_operation = format!("/repos/{repository}/contents/{nested_path}");
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));
    }

    #[test]
    fn g0_workflow_source_enforces_size_cap_and_path_component_rules() {
        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        let collector = &inventory.collector_snapshot;
        let repository = &collector.repositories[0];
        let workflow = &repository.workflows[0];
        let mut oversized_source = workflow.source.clone();
        oversized_source.bytes_base64 = "A".repeat(G0_MAX_WORKFLOW_SOURCE_BASE64_BYTES + 1);
        oversized_source.byte_length = (G0_MAX_WORKFLOW_SOURCE_BYTES + 1) as u64;
        let dependencies = workflow
            .reusable_workflows
            .iter()
            .chain(workflow.actions.iter())
            .chain(workflow.scanners.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut findings = Vec::new();
        let plan = check_g0_workflow_source(
            "workflow.source",
            &oversized_source,
            G0WorkflowSourceContext {
                repository: &repository.repository,
                default_branch_sha: &repository.default_branch_sha,
                dependencies: &dependencies,
                raw_objects: &collector.raw_objects,
                requests: &collector.requests,
            },
            &mut findings,
        );
        assert!(plan.is_none());
        assert!(g0_codes(&findings).contains("g0-workflow-source"));

        inventory.collector_snapshot.repositories[0].workflows[0].source = oversized_source;
        refresh_typed_inventory_bytes(&mut inventory);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-workflow-source"));

        let oversized_decoded = BASE64.encode(vec![b'x'; G0_MAX_WORKFLOW_SOURCE_BYTES + 1]);
        assert_eq!(oversized_decoded.len(), G0_MAX_WORKFLOW_SOURCE_BASE64_BYTES);
        assert!(!g0_workflow_source_size_within_limit(
            &oversized_decoded,
            G0_MAX_WORKFLOW_SOURCE_BYTES as u64
        ));

        assert!(g0_workflow_file_path(".github/workflows/ci..yml"));
        assert!(g0_relative_source_path_is_safe("my..action/action.yml"));
        assert!(!g0_workflow_file_path(".github/workflows/../ci.yml"));
        assert!(!g0_relative_source_path_is_safe("my-action/../action.yml"));
    }

    #[test]
    fn g0_artifact_checksum_is_not_compared_to_listing_body_hash() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let repository = &inventory.collector_snapshot.repositories[0];
        let artifact = &repository.artifacts[0];
        let raw = inventory
            .collector_snapshot
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == artifact.raw_object_refs[0])
            .expect("artifact-list raw response");
        assert_ne!(artifact.digest, raw.sha256);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(
            !g0_codes(&findings).contains("g0-artifact-digest"),
            "provider-reported archive digest is not the listing response-body digest"
        );
    }

    #[test]
    fn g0_pr_runs_require_current_head_and_unique_snapshot_identities() {
        let (manifest, mut snapshot, mut inventory) = complete_g0_fixture();
        let stale_head = sha('z');
        let pr = &mut inventory.collector_snapshot.repositories[0].open_prs[0];
        pr.workflow_bindings[0].source_sha = stale_head.clone();
        pr.required_check_producers[0].source_sha = stale_head.clone();
        snapshot.repositories[0].open_prs[0].executions[0].trigger_source_sha = stale_head;
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        let codes = g0_codes(&findings);
        assert!(codes.contains("g0-pr-workflow-binding"));
        assert!(codes.contains("g0-pr-check-producer"));

        let (manifest, mut snapshot, inventory) = complete_g0_fixture();
        let mut duplicate = snapshot.repositories[0].open_prs[0].executions[0].clone();
        duplicate.actual_checkout_sha = sha('z');
        snapshot.repositories[0].open_prs[0]
            .executions
            .push(duplicate);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-pr-execution-duplicate"));

        let (manifest, mut snapshot, inventory) = complete_g0_fixture();
        let duplicate = snapshot.repositories[0].main_executions[0].clone();
        snapshot.repositories[0].main_executions.push(duplicate);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-main-execution-duplicate"));
    }

    #[test]
    fn g0_policy_fails_closed_for_workspace_relative_source_action() {
        let (manifest, snapshot, mut inventory) = complete_g0_fixture();
        let workflow_bytes = b"on: [push, pull_request]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: ./source/.github/actions/setup-velnor-workflow\n";
        let raw_id = {
            let workflow = &mut inventory.collector_snapshot.repositories[0].workflows[0].source;
            let digest = digest_bytes(workflow_bytes);
            workflow.byte_length = workflow_bytes.len() as u64;
            workflow.bytes_base64 = BASE64.encode(workflow_bytes);
            workflow.sha256 = digest.clone();
            workflow.storage_ref = format!(
                "sha256://{}",
                digest.strip_prefix("sha256:").expect("digest has prefix")
            );
            workflow.raw_object_refs[0].clone()
        };
        let raw = inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == raw_id)
            .expect("workflow source raw object");
        let digest = digest_bytes(workflow_bytes);
        raw.byte_length = workflow_bytes.len() as u64;
        raw.bytes_base64 = BASE64.encode(workflow_bytes);
        raw.sha256 = digest.clone();
        raw.original_byte_length = workflow_bytes.len() as u64;
        raw.original_sha256 = digest.clone();
        raw.storage_ref = format!(
            "sha256://{}",
            digest.strip_prefix("sha256:").expect("digest has prefix")
        );
        raw.original_storage_ref = raw.storage_ref.clone();
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-workflow-derivation"),
            "workspace-relative source action must remain blocked: {findings:?}"
        );
    }

    #[test]
    fn g0_action_execution_semantics_remain_blocked() {
        let (_, snapshot, inventory) = complete_g0_fixture();
        let repository = &snapshot.repositories[0];
        let workflow = inventory.collector_snapshot.repositories[0].workflows[0]
            .source
            .clone();
        let raw_id = workflow.raw_object_refs[0].clone();
        let workflow_bytes = format!(
            "on: [push, pull_request]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@{}\n",
            workflow.source_sha
        );
        let workflow_digest = digest_bytes(workflow_bytes.as_bytes());
        let mut workflow = workflow;
        workflow.byte_length = workflow_bytes.len() as u64;
        workflow.bytes_base64 = BASE64.encode(workflow_bytes.as_bytes());
        workflow.sha256 = workflow_digest.clone();
        workflow.storage_ref = format!(
            "sha256://{}",
            workflow_digest
                .strip_prefix("sha256:")
                .expect("workflow digest has prefix")
        );
        let mut raw_objects = inventory.collector_snapshot.raw_objects.clone();
        let root_raw = raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == raw_id)
            .expect("workflow source raw object");
        root_raw.byte_length = workflow.byte_length;
        root_raw.bytes_base64 = workflow.bytes_base64.clone();
        root_raw.sha256 = workflow.sha256.clone();
        root_raw.storage_ref = workflow.storage_ref.clone();
        root_raw.original_byte_length = workflow.byte_length;
        root_raw.original_sha256 = workflow.sha256.clone();
        root_raw.original_storage_ref = workflow.storage_ref.clone();

        let action_bytes = b"name: checkout\n";
        let action_digest = digest_bytes(action_bytes);
        let mut action = workflow.clone();
        action.repository = "actions/checkout".to_owned();
        action.path = "action.yml".to_owned();
        action.source_url = format!(
            "https://github.com/{}/blob/{}/{}",
            action.repository, action.source_sha, action.path
        );
        action.bytes_base64 = BASE64.encode(action_bytes);
        action.byte_length = action_bytes.len() as u64;
        action.sha256 = action_digest.clone();
        action.storage_ref = format!(
            "sha256://{}",
            action_digest
                .strip_prefix("sha256:")
                .expect("action digest has prefix")
        );
        action.raw_object_refs = Vec::new();
        let dependencies = [G0WorkflowDependency {
            kind: "action".to_owned(),
            source: action,
        }];

        let mut findings = Vec::new();
        let plan = check_g0_workflow_source(
            "workflow.source",
            &workflow,
            G0WorkflowSourceContext {
                repository: &repository.repository,
                default_branch_sha: &repository.default_branch_sha,
                dependencies: &dependencies,
                raw_objects: &raw_objects,
                requests: &inventory.collector_snapshot.requests,
            },
            &mut findings,
        )
        .expect("source-bound workflow plan");
        assert!(plan.has_action_steps);
        assert!(g0_codes(&findings).contains("g0-action-semantics-unverified"));
    }

    #[test]
    fn complete_g0_fixture_round_trips_through_public_check_paths() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
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
        assert_eq!(report.status, "fail");
        assert_eq!(report.mode, "offline");

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
        let root_yaml = "on: [push]\njobs:\n  scan:\n    uses: ./.github/workflows/reusable.yml\n";
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
                object_kind: "github-workflow-source".to_owned(),
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
        let mut child_request = inventory
            .collector_snapshot
            .requests
            .iter()
            .find(|request| request.response_raw_ref == root_raw_id)
            .expect("root workflow source request")
            .clone();
        child_request.request_id = child_request_id;
        child_request.endpoint_or_operation =
            format!("/repos/{repository}/contents/.github/workflows/reusable.yml");
        child_request.query_base64 = BASE64.encode(format!("ref={}", sha('a')).as_bytes());
        child_request.query_sha256 = digest_bytes(format!("ref={}", sha('a')).as_bytes());
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
    fn nested_child_workflows_bind_exact_parent_sources_when_job_ids_reuse_root_name() {
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
        let root_yaml = b"on: [push]\njobs:\n  scan:\n    uses: tailrocks/velnor/.github/workflows/reusable.yml@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        root_source.bytes_base64 = BASE64.encode(root_yaml);
        root_source.byte_length = root_yaml.len() as u64;
        root_source.sha256 = digest_bytes(root_yaml);
        root_source.source_url = format!(
            "https://github.com/{repository}/blob/{}/{}",
            root_source.source_sha, root_source.path
        );

        let parent_workflow_path = root_source.path.clone();
        let parent_source_sha = root_source.revision.clone();
        let mut reusable = root_source.clone();
        reusable.path = ".github/workflows/reusable.yml".to_owned();
        reusable.revision = sha('b');
        reusable.source_sha = sha('b');
        reusable.source_url = format!(
            "https://github.com/{repository}/blob/{}/{}",
            reusable.source_sha, reusable.path
        );
        let reusable_yaml = b"on: {workflow_call: {}}\njobs:\n  scan:\n    uses: tailrocks/velnor/.github/workflows/deep.yml@cccccccccccccccccccccccccccccccccccccccc\n";
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

        let dependencies = workflow
            .reusable_workflows
            .iter()
            .chain(workflow.actions.iter())
            .chain(workflow.scanners.iter())
            .cloned()
            .collect::<Vec<_>>();
        let plan = derive_workflow_plan(&workflow.source, &dependencies)
            .expect("derive recursive plan with reused logical job ID");
        assert!(plan.child_edges.iter().any(|edge| {
            edge.workload_id == "scan"
                && edge.root_workload_id == "scan"
                && edge.parent_workflow_path == ".github/workflows/reusable.yml"
        }));
        let mut plan_findings = Vec::new();
        check_g0_derived_plan(
            &repository,
            manifest_repo,
            workflow,
            &plan,
            &mut plan_findings,
        );
        assert!(
            !plan_findings
                .iter()
                .any(|finding| finding.code == "g0-workflow-child"),
            "nested edge with reused job name was treated as direct: {plan_findings:?}"
        );
        assert!(plan.jobs[0].uses_reusable_workflow);

        let mut missing_reusable_flag = plan.clone();
        missing_reusable_flag.jobs[0].uses_reusable_workflow = false;
        plan_findings.clear();
        check_g0_derived_plan(
            &repository,
            manifest_repo,
            workflow,
            &missing_reusable_flag,
            &mut plan_findings,
        );
        assert!(
            plan_findings
                .iter()
                .any(|finding| finding.code == "g0-workflow-child"),
            "reusable job metadata must match its direct workflow_call edge: {plan_findings:?}"
        );

        let mut wrong_child_workload = plan.clone();
        wrong_child_workload
            .child_edges
            .iter_mut()
            .find(|edge| edge.parent_workflow_path == ".github/workflows/reusable.yml")
            .expect("nested edge from reusable source")
            .workload_id = "unreviewed".to_owned();
        plan_findings.clear();
        check_g0_derived_plan(
            &repository,
            manifest_repo,
            workflow,
            &wrong_child_workload,
            &mut plan_findings,
        );
        assert!(
            plan_findings.iter().any(|finding| {
                finding.field == "workflow.source/child_edges.workload_id"
                    && finding.message.contains("unreviewed")
            }),
            "edge workload must be a job in its immutable parent source: {plan_findings:?}"
        );

        let mut child_execution = ExecutionObservation {
            run_id: 101,
            run_attempt: 2,
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
        let child_a = ChildWorkflowObservation {
            parent_run_id: child_execution.run_id,
            parent_run_attempt: child_execution.run_attempt,
            parent_repository: repository.clone(),
            parent_workflow_path: parent_workflow_path.clone(),
            parent_source_sha: parent_source_sha.clone(),
            run_id: child_execution.run_id,
            run_attempt: child_execution.run_attempt,
            repository: repository.clone(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('b'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            source_url: child_execution.run_url.clone(),
        };
        let child_b = ChildWorkflowObservation {
            parent_run_id: child_a.run_id,
            parent_run_attempt: child_a.run_attempt,
            parent_repository: child_a.repository.clone(),
            parent_workflow_path: child_a.workflow_path.clone(),
            parent_source_sha: child_a.source_sha.clone(),
            run_id: child_execution.run_id,
            run_attempt: child_execution.run_attempt,
            repository: repository.clone(),
            workflow_path: ".github/workflows/deep.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('c'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            source_url: child_execution.run_url.clone(),
        };
        child_execution.child_workflows = vec![child_a.clone(), child_b.clone()];
        let links = [child_a, child_b]
            .into_iter()
            .map(|child| ChildWorkflowLink {
                parent_run_id: child.parent_run_id,
                parent_run_attempt: child.parent_run_attempt,
                parent_repository: child.parent_repository,
                parent_workflow_path: child.parent_workflow_path,
                parent_source_sha: child.parent_source_sha,
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
            child_workflow_links: links,
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

        let mut rerun_execution = child_execution.clone();
        rerun_execution.run_attempt = 2;
        let mut rerun_record = record.clone();
        rerun_record.child_workflow_links[0].parent_run_attempt = 1;
        findings.clear();
        check_authoritative_children_from_source(
            manifest_repo,
            Some(&inventory),
            &rerun_record,
            &rerun_execution,
            &mut findings,
        );
        assert!(
            findings.iter().any(|finding| {
                matches!(
                    finding.code.as_str(),
                    "child-run-mismatch" | "missing-child-run"
                )
            }),
            "a child from parent attempt 1 satisfied parent attempt 2: {findings:?}"
        );

        let mut wrong_nested_attempt = child_execution.clone();
        wrong_nested_attempt.child_workflows[1].parent_run_attempt = 1;
        findings.clear();
        check_authoritative_children_from_source(
            manifest_repo,
            Some(&inventory),
            &record,
            &wrong_nested_attempt,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "child-run-mismatch"));

        let mut observation_findings = Vec::new();
        check_execution_observation(&repository, &child_execution, &mut observation_findings);
        assert!(!observation_findings
            .iter()
            .any(|finding| finding.code == "child-run-conclusion"));

        let mut duplicate_links = record.clone();
        duplicate_links.child_workflow_links[1] = duplicate_links.child_workflow_links[0].clone();
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
        wrong_parent.child_workflows[1].parent_run_id = 999;
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

    #[test]
    fn repeated_nested_child_targets_join_by_parent_run_identity() {
        let (manifest, _, inventory) = complete_g0_fixture();
        let mut manifest = manifest.repositories[0].clone();
        let root_source = inventory.collector_snapshot.repositories[0].workflows[0]
            .source
            .clone();
        let repository = root_source.repository.clone();
        let parent_a_path = ".github/workflows/parent-a.yml";
        let parent_b_path = ".github/workflows/parent-b.yml";
        let child_path = ".github/workflows/deep.yml";
        let mut parent_a_source = root_source.clone();
        parent_a_source.path = parent_a_path.to_owned();
        parent_a_source.revision = sha('b');
        parent_a_source.source_sha = sha('b');
        let mut parent_b_source = root_source.clone();
        parent_b_source.path = parent_b_path.to_owned();
        parent_b_source.revision = sha('c');
        parent_b_source.source_sha = sha('c');
        let parent_a = ChildWorkflowObservation {
            parent_run_id: 101,
            parent_run_attempt: 2,
            parent_repository: repository.clone(),
            parent_workflow_path: root_source.path.clone(),
            parent_source_sha: root_source.revision.clone(),
            run_id: 101,
            run_attempt: 2,
            repository: repository.clone(),
            workflow_path: parent_a_path.to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('b'),
            ..Default::default()
        };
        let parent_b = ChildWorkflowObservation {
            parent_run_id: 101,
            parent_run_attempt: 2,
            parent_repository: repository.clone(),
            parent_workflow_path: root_source.path.clone(),
            parent_source_sha: root_source.revision.clone(),
            run_id: 101,
            run_attempt: 2,
            repository: repository.clone(),
            workflow_path: parent_b_path.to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('c'),
            ..Default::default()
        };
        let child = ChildWorkflowObservation {
            parent_run_id: parent_b.run_id,
            parent_run_attempt: parent_b.run_attempt,
            parent_repository: parent_b.repository.clone(),
            parent_workflow_path: parent_b.workflow_path.clone(),
            parent_source_sha: parent_b.source_sha.clone(),
            run_id: 101,
            run_attempt: 2,
            repository: repository.clone(),
            workflow_path: child_path.to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('d'),
            ..Default::default()
        };
        let execution = ExecutionObservation {
            run_id: 101,
            run_attempt: 2,
            workflow_path: root_source.path.clone(),
            workflow_revision: root_source.revision.clone(),
            child_workflows: vec![parent_a.clone(), parent_b.clone(), child.clone()],
            ..Default::default()
        };
        let edge = |root_job: &str,
                    workload: &str,
                    target_path: &str,
                    target_sha: char,
                    parent_path: &str,
                    parent_sha: String| crate::g0_workflow::DerivedChildEdge {
            workload_id: workload.to_owned(),
            root_workload_id: root_job.to_owned(),
            repository: repository.clone(),
            workflow_path: target_path.to_owned(),
            event: "workflow_call".to_owned(),
            relation: "reusable_workflow".to_owned(),
            source_sha: sha(target_sha),
            parent_repository: repository.clone(),
            parent_workflow_path: parent_path.to_owned(),
            parent_source_sha: parent_sha,
        };
        let direct_a = edge(
            "job_a",
            "job_a",
            parent_a_path,
            'b',
            &root_source.path,
            root_source.source_sha.clone(),
        );
        let direct_b = edge(
            "job_b",
            "job_b",
            parent_b_path,
            'c',
            &root_source.path,
            root_source.source_sha.clone(),
        );
        let nested_a = edge("job_a", "nested", child_path, 'd', parent_a_path, sha('b'));
        let nested_b = edge("job_b", "nested", child_path, 'd', parent_b_path, sha('c'));
        let plan = DerivedWorkflowPlan {
            jobs: Vec::new(),
            child_edges: vec![direct_a, direct_b, nested_a, nested_b],
            events: BTreeSet::new(),
            has_action_steps: false,
        };
        let reusable_workflows = vec![
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: parent_a_source,
            },
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: parent_b_source,
            },
        ];
        let expected_job = |job_id: &str, path: &str| ExpectedJobSpec {
            job_id: job_id.to_owned(),
            workload_id: job_id.to_owned(),
            provider: "github".to_owned(),
            platform: "ubuntu-24.04".to_owned(),
            architecture: "amd64".to_owned(),
            required: true,
            child_workflow: Some(ChildWorkflowSpec {
                repository: repository.clone(),
                workflow_path: path.to_owned(),
                event: "workflow_call".to_owned(),
            }),
        };
        manifest.expected_jobs = vec![
            expected_job("job_a", parent_a_path),
            expected_job("job_b", parent_b_path),
        ];
        let edge_match_context = G0ChildEdgeMatchContext {
            repository: &repository,
            provider: "github",
            execution: &execution,
            root_source: &root_source,
            manifest: &manifest,
            plan: &plan,
            reusable_workflows: &reusable_workflows,
        };
        let nested_candidates = plan
            .child_edges
            .iter()
            .filter(|candidate| {
                g0_child_edge_matches_observation(&child, candidate, &edge_match_context)
            })
            .collect::<Vec<_>>();
        assert_eq!(nested_candidates.len(), 1);
        assert_eq!(nested_candidates[0].root_workload_id, "job_b");
    }

    fn minimal_execution() -> ExecutionObservation {
        ExecutionObservation {
            run_id: 1,
            run_attempt: 1,
            run_url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
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
        let mut workflow_derivation = WorkflowDerivationContext::default();
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
            &mut workflow_derivation,
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
            "accept": "application/vnd.github+json",
            "query_base64": BASE64.encode(b"page=1"),
            "variables_base64": BASE64.encode(b"{}"),
            "query_sha256": digest_bytes(b"page=1"),
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
    fn rest_page_number_and_size_must_match_the_decoded_query() {
        let (_, _, inventory) = complete_g0_fixture();
        let request = inventory.collector_snapshot.requests[1].clone();
        let page_one = b"page=1&per_page=100";
        assert!(g0_request_semantics(&request, Some(page_one), Some(b"{}")));

        let repository = &inventory.collector_snapshot.repositories[0].repository;
        let mut branch_rules_request = request.clone();
        branch_rules_request.endpoint_or_operation =
            format!("/repos/{repository}/rules/branches/main");
        assert!(g0_request_semantics(
            &branch_rules_request,
            Some(page_one),
            Some(b"{}")
        ));

        let mut mislabeled_page = request.clone();
        mislabeled_page.page.number = 2;
        assert!(!g0_request_semantics(
            &mislabeled_page,
            Some(page_one),
            Some(b"{}")
        ));

        let mut mislabeled_page_size = request.clone();
        mislabeled_page_size.page.per_page = Some(50);
        assert!(!g0_request_semantics(
            &mislabeled_page_size,
            Some(page_one),
            Some(b"{}")
        ));

        let page_two = b"page=2&per_page=100";
        assert!(!g0_request_semantics(&request, Some(page_two), Some(b"{}")));

        let repository = &inventory.collector_snapshot.repositories[0].repository;
        for endpoint in [
            format!("/repos/{repository}/actions/runs/42/jobs"),
            format!("/repos/{repository}/actions/runs/42/attempts/2/jobs"),
        ] {
            let mut jobs_request = request.clone();
            jobs_request.endpoint_or_operation = endpoint;
            assert!(g0_request_semantics(
                &jobs_request,
                Some(page_one),
                Some(b"{}")
            ));

            jobs_request.page.number = 2;
            assert!(g0_request_semantics(
                &jobs_request,
                Some(page_two),
                Some(b"{}")
            ));
            jobs_request.page.number = 1;
            assert!(!g0_request_semantics(
                &jobs_request,
                Some(page_two),
                Some(b"{}")
            ));
            jobs_request.page.number = 2;
            jobs_request.page.per_page = Some(50);
            assert!(!g0_request_semantics(
                &jobs_request,
                Some(page_two),
                Some(b"{}")
            ));
        }
    }

    #[test]
    fn g0_pagination_missing_next_page_fails_closed() {
        let mut collector = minimal_g0_collector();
        collector.requests.push(G0RequestRecord {
            request_id: "request-1".to_owned(),
            api: G0ApiKind::Rest,
            method: "GET".to_owned(),
            endpoint_or_operation:
                "/repos/tailrocks/velnor/actions/runs/1/artifacts".to_owned(),
            accept: "application/vnd.github+json".to_owned(),
            query_base64: BASE64.encode(b"page=1&per_page=100"),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: digest_bytes(b"page=1&per_page=100"),
            variables_sha256: digest_bytes(b"{}"),
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            http_status: 200,
            api_request_id: "request-id".to_owned(),
            rate_limit_ref: "collector.rate_limit".to_owned(),
            page: G0Page {
                number: 1,
                per_page: Some(100),
                link_next: Some(
                    "https://api.github.com/repos/tailrocks/velnor/actions/runs/1/artifacts?page=2&per_page=100"
                        .to_owned(),
                ),
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
        second.query_base64 = BASE64.encode(b"page=2&per_page=100");
        second.query_sha256 = digest_bytes(b"page=2&per_page=100");
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
        assert!(
            !findings.iter().any(|finding| {
                matches!(
                    finding.code.as_str(),
                    "g0-pagination" | "g0-request-incomplete"
                )
            }),
            "complete page stream rejected: {findings:?}"
        );
        let mut missing_next_page = collector.clone();
        missing_next_page.requests[0].page.link_next = None;
        findings.clear();
        check_g0_request_provenance(&missing_next_page, &mut findings);
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
        let mut workflow_derivation = WorkflowDerivationContext::default();
        let mut findings = Vec::new();
        check_g0_dependency_graph(
            &manifest,
            &collector,
            &raw_ids,
            &mut workflow_derivation,
            &mut findings,
        );
        check_g0_model_session(&collector, &raw_ids, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-dependency-edge"));
        assert!(!findings
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
    fn root_run_url_must_bind_repository_and_run_id() {
        let execution = minimal_execution();
        let mut findings = Vec::new();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(!g0_codes(&findings).contains("run-identity"));

        for url in [
            "https://github.com/owner/other/actions/runs/1",
            "https://github.com/owner/repo/actions/runs/2",
            "https://example.com/owner/repo/actions/runs/1",
        ] {
            let mut foreign = execution.clone();
            foreign.run_url = url.to_owned();
            findings.clear();
            check_execution_observation("owner/repo", &foreign, &mut findings);
            assert!(
                g0_codes(&findings).contains("run-identity"),
                "accepted noncanonical root run URL {url}"
            );
        }
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
        execution.child_workflows.push(ChildWorkflowObservation {
            parent_run_id: 1,
            parent_run_attempt: 1,
            parent_repository: "owner/repo".to_owned(),
            parent_workflow_path: ".github/workflows/ci.yml".to_owned(),
            parent_source_sha: execution.workflow_revision.clone(),
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
    fn trusted_live_mode_keeps_manifest_projection_blocker() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: Stage::G0.as_str().to_owned(),
            records: Vec::new(),
            reviewer_attestation: None,
            g0_inventory: Some(inventory),
        };

        let report = check_documents(
            Stage::G0,
            &manifest,
            &snapshot,
            &evidence,
            None,
            CheckMode::TrustedLive,
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "manifest-projection-unverified"));
        for blocker in [
            "check-id-response-unverified",
            "g0-api-row-response-unverified",
            "g0-request-api-version-unverified",
            "g0-check-id-response-unverified",
            "g0-artifact-row-unverified",
            "g0-artifact-attempt-unverified",
            "g0-artifact-archive-unverified",
            "g0-artifact-source-digest-unverified",
            "g0-pagination-response-unverified",
            "required-check-inventory-unverified",
            "g0-child-target-binding-unverified",
        ] {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding.code == blocker),
                "missing explicit live G0 blocker {blocker}"
            );
        }
        assert_eq!(report.status, "fail");

        let mut g2_evidence = evidence;
        g2_evidence.stage = Stage::G2.as_str().to_owned();
        g2_evidence.g0_inventory = None;
        let g2_report = check_documents(
            Stage::G2,
            &manifest,
            &snapshot,
            &g2_evidence,
            None,
            CheckMode::TrustedLive,
        );
        assert!(g2_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-release-producer-unverified"));
        assert!(g2_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-child-target-binding-unverified"));
        assert_eq!(g2_report.status, "fail");
    }

    #[test]
    fn g0_empty_main_execution_inventory_is_explicit_and_never_trusted_live() {
        let (manifest, mut snapshot, mut inventory) = complete_g0_fixture();
        snapshot.repositories[0].main_executions.clear();
        inventory.collector_snapshot.repositories[0]
            .main_checks
            .clear();
        refresh_typed_inventory_bytes(&mut inventory);

        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(g0_codes(&findings).contains("g0-check-inventory"));
        assert!(!findings.iter().any(|finding| {
            finding.code == "g0-check-snapshot-mismatch"
                && finding.repository.as_deref() == Some(&manifest.repositories[0].repository)
        }));

        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: Stage::G0.as_str().to_owned(),
            records: Vec::new(),
            reviewer_attestation: None,
            g0_inventory: Some(inventory),
        };
        let report = check_documents(
            Stage::G0,
            &manifest,
            &snapshot,
            &evidence,
            None,
            CheckMode::TrustedLive,
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-check-inventory"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "required-check-inventory-unverified"));
        assert_eq!(report.status, "fail");
    }

    #[test]
    fn ruleset_rows_and_classic_only_shape_do_not_verify_effective_required_checks() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let mut cases = vec![inventory.clone()];

        let mut disabled_ruleset_response = inventory.clone();
        let ruleset_raw_id = disabled_ruleset_response.collector_snapshot.repositories[0].rulesets
            [0]
        .raw_object_refs[0]
            .clone();
        let disabled_bytes = br#"{"enforcement":"disabled","target":"branch"}"#;
        let disabled_digest = digest_bytes(disabled_bytes);
        let disabled_raw = disabled_ruleset_response
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == ruleset_raw_id)
            .expect("ruleset raw response");
        disabled_raw.bytes_base64 = BASE64.encode(disabled_bytes);
        disabled_raw.byte_length = disabled_bytes.len() as u64;
        disabled_raw.sha256 = disabled_digest.clone();
        disabled_raw.storage_ref = format!(
            "sha256://{}",
            disabled_digest
                .strip_prefix("sha256:")
                .expect("digest prefix")
        );
        disabled_raw.original_sha256 = disabled_digest.clone();
        disabled_raw.original_byte_length = disabled_bytes.len() as u64;
        disabled_raw.original_storage_ref = disabled_raw.storage_ref.clone();
        refresh_typed_inventory_bytes(&mut disabled_ruleset_response);
        cases.push(disabled_ruleset_response);

        let mut classic_only = inventory;
        classic_only.collector_snapshot.repositories[0]
            .rulesets
            .clear();
        refresh_typed_inventory_bytes(&mut classic_only);
        cases.push(classic_only);

        for candidate in cases {
            let evidence = EvidenceDocument {
                schema_version: EVIDENCE_SCHEMA_VERSION,
                manifest_id: manifest.manifest_id.clone(),
                snapshot_id: snapshot.snapshot_id.clone(),
                stage: Stage::G0.as_str().to_owned(),
                records: Vec::new(),
                reviewer_attestation: None,
                g0_inventory: Some(candidate),
            };
            let report = check_documents(
                Stage::G0,
                &manifest,
                &snapshot,
                &evidence,
                None,
                CheckMode::TrustedLive,
            );
            assert!(report
                .findings
                .iter()
                .any(|finding| finding.code == "required-check-inventory-unverified"));
            assert_eq!(report.status, "fail");
        }
    }

    #[test]
    fn caller_digest_and_sha_only_child_target_do_not_authorize_trusted_live() {
        let (mut manifest, snapshot, mut inventory) = complete_g0_fixture();
        let projection = serde_json::to_value(&manifest.repositories).expect("manifest rows");
        manifest.source.digest = digest_bytes(canonical_json(&projection).as_bytes());

        let repository = inventory.collector_snapshot.repositories[0]
            .repository
            .clone();
        let source_sha = inventory.collector_snapshot.repositories[0]
            .default_branch_sha
            .clone();
        let child_node_id = format!("child:{repository}:scan");
        inventory
            .collector_snapshot
            .dependency_graph
            .nodes
            .push(G0GraphNode {
                id: child_node_id.clone(),
                kind: "child".to_owned(),
                repository: repository.clone(),
                workload_id: "scan".to_owned(),
                applicability: "required".to_owned(),
                source_sha: source_sha.clone(),
                source_ref: "refs/heads/main".to_owned(),
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            });
        inventory
            .collector_snapshot
            .dependency_graph
            .edges
            .push(G0GraphEdge {
                from: format!("workload:{repository}:scan"),
                to: child_node_id,
                kind: "workload-to-child".to_owned(),
                required: true,
                source_sha: source_sha.clone(),
                source_ref: "refs/heads/main".to_owned(),
                target_source_sha: source_sha,
                target_source_ref: "refs/heads/main".to_owned(),
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            });
        refresh_typed_inventory_bytes(&mut inventory);

        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: Stage::G0.as_str().to_owned(),
            records: Vec::new(),
            reviewer_attestation: None,
            g0_inventory: Some(inventory),
        };
        let report = check_documents(
            Stage::G0,
            &manifest,
            &snapshot,
            &evidence,
            None,
            CheckMode::TrustedLive,
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "manifest-projection-unverified"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-child-target-binding-unverified"));
        assert_eq!(report.status, "fail");
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
            has_action_steps: false,
        };
        let raw_ids = workflow
            .source_jobs
            .iter()
            .flat_map(|job| job.raw_object_refs.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut findings = Vec::new();
        check_g0_source_jobs(
            &manifest_repo.repository,
            Some(manifest_repo),
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
