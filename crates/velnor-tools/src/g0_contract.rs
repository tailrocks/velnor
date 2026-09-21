//! Strict, collector-owned G0 evidence contract.
//!
//! These types deliberately model observations and their raw-object/request
//! provenance separately from result records.  A record cannot manufacture a
//! repository census, expected check set, dependency graph, model session, or
//! access state.  The live collector owns construction; the checker owns the
//! deterministic validation boundary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0InventoryEvidence {
    pub collector_snapshot: G0CollectorSnapshot,
    /// Canonical JSON bytes produced by the collector.  The checker decodes
    /// and parses these bytes instead of trusting a digest over the parsed
    /// caller object.
    pub collector_snapshot_bytes_base64: String,
    /// External digest of canonical `collector_snapshot` bytes.  It is kept
    /// outside the object to avoid a self-referential hash cycle.
    pub collector_snapshot_sha256: String,
    /// Content-addressed immutable storage location for the exact snapshot
    /// bytes.  The collector owns this object; a path or mutable memory key is
    /// not an authoritative provenance binding.
    pub collector_snapshot_storage_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0CollectorSnapshot {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub manifest_id: String,
    pub phase: String,
    pub observed_at_utc: String,
    pub completed_at_utc: String,
    pub collector: G0CollectorIdentity,
    pub auth: G0AuthIdentity,
    pub rate_limit: G0RateLimitObservation,
    pub requests: Vec<G0RequestRecord>,
    pub raw_objects: Vec<G0RawObjectRef>,
    pub repositories: Vec<G0RepositoryInventory>,
    pub reconciliation: G0RevisionReconciliation,
    pub dependency_graph: G0DependencyGraph,
    pub model_session: G0ModelSession,
    pub access: Vec<G0AccessObservation>,
    pub workload_artifact: G0ArtifactReference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0CollectorIdentity {
    pub name: String,
    pub revision: String,
    pub mode: String,
    pub api_base: String,
    pub api_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0AuthIdentity {
    pub provider: String,
    pub viewer_id: String,
    pub viewer_login: String,
    pub safe_scopes: Vec<String>,
    pub secret_excluded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RateLimitObservation {
    pub api: String,
    pub limit: u64,
    pub remaining: u64,
    pub used: u64,
    pub reset_at_utc: String,
    pub observed_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RequestRecord {
    pub request_id: String,
    pub api: G0ApiKind,
    pub method: String,
    pub endpoint_or_operation: String,
    /// Canonical read-only query text (REST query or GraphQL document),
    /// encoded so its digest can be recomputed without storing secrets. REST
    /// payloads use the acquisition wire form `key\0value\0` in sorted key
    /// order; URL query strings embedded in `endpoint_or_operation` remain
    /// ordinary `key=value&...` URLs and are validated separately.
    pub query_base64: String,
    /// Canonical, redacted query variables; credentials are forbidden.
    pub variables_base64: String,
    pub query_sha256: String,
    pub variables_sha256: String,
    pub auth_identity_ref: String,
    pub started_at_utc: String,
    pub completed_at_utc: String,
    pub http_status: u16,
    pub api_request_id: String,
    pub rate_limit_ref: String,
    pub page: G0Page,
    pub response_raw_ref: String,
    pub error_raw_ref: Option<String>,
    pub state: G0RequestState,
    pub complete: bool,
    pub truncation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum G0ApiKind {
    Rest,
    Graphql,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum G0RequestState {
    Complete,
    EmptyComplete,
    Forbidden,
    NotFound,
    RateLimited,
    TransportError,
    Malformed,
    Truncated,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0Page {
    pub number: u32,
    pub per_page: u32,
    pub link_next: Option<String>,
    pub cursor_in: Option<String>,
    pub cursor_out: Option<String>,
    pub has_next_page: bool,
    pub items_returned: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RawObjectRef {
    pub raw_id: String,
    pub request_id: String,
    pub object_kind: String,
    pub canonicalization: String,
    pub sha256: String,
    pub byte_length: u64,
    /// Immutable response bytes supplied by the collector/store.  The
    /// checker recomputes `sha256` and `byte_length` from this value.
    pub bytes_base64: String,
    pub media_type: String,
    pub storage_ref: String,
    /// Digest and immutable storage reference for the exact provider response
    /// before credential masking or safe-byte canonicalization. The producer
    /// store computes these values from the original bytes; a caller cannot
    /// make them authoritative by supplying strings alone.
    pub original_sha256: String,
    pub original_byte_length: u64,
    pub original_storage_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0ArtifactReference {
    pub name: String,
    pub schema: String,
    pub source_url: String,
    pub sha256: String,
    /// The artifact bytes are reopened from this external CAS object.  The
    /// reference is outside the artifact object so it cannot be a hash-cycle
    /// or an unverified caller path.
    pub storage_ref: String,
    pub source_revision: String,
    pub source_digest: String,
    pub observed_at_utc: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RepositoryInventory {
    pub repository: String,
    pub repository_id: u64,
    pub default_branch: String,
    pub default_branch_sha: String,
    pub rulesets: Vec<G0RulesetInventory>,
    pub workflows: Vec<G0WorkflowInventory>,
    /// Complete artifact census for the source-bound runs retained by the
    /// collector.  Artifact rows are not inferred from result records.
    pub artifacts: Vec<G0ArtifactObservation>,
    pub open_prs: Vec<G0PullRequestInventory>,
    pub main_checks: Vec<G0CheckProducer>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0ArtifactObservation {
    pub artifact_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub run_head_sha: String,
    pub name: String,
    /// Digest of the downloadable artifact archive reported by GitHub. This
    /// is intentionally distinct from the SHA-256 of the paginated API
    /// response stored in `G0RawObjectRef`.
    pub digest: String,
    pub expired: bool,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RulesetInventory {
    pub ruleset_id: u64,
    pub name: String,
    pub source_url: String,
    pub complete: bool,
    pub required_checks: Vec<G0RequiredCheckPolicy>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RequiredCheckPolicy {
    pub context: String,
    pub app_id: String,
    pub ruleset_id: u64,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0WorkflowInventory {
    pub source: G0WorkflowSource,
    pub events: Vec<String>,
    /// Exact jobs derived from the immutable workflow source.  This is a
    /// source fact, not a projection of observed run/job rows.
    pub source_jobs: Vec<G0SourceJob>,
    pub reusable_workflows: Vec<G0WorkflowDependency>,
    pub actions: Vec<G0WorkflowDependency>,
    pub scanners: Vec<G0WorkflowDependency>,
    pub generated_state: G0ArtifactReference,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0SourceJob {
    pub job_id: String,
    pub workload_id: String,
    pub provider: String,
    pub platform: String,
    pub architecture: String,
    pub required: bool,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0WorkflowDependency {
    pub kind: String,
    pub source: G0WorkflowSource,
}

/// Immutable workflow/action bytes captured from the reviewed repository or a
/// recursively referenced source.  The checker hashes these exact bytes and
/// parses them; a URL, blob SHA, or caller-provided plan alone is insufficient.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0WorkflowSource {
    pub repository: String,
    pub path: String,
    pub revision: String,
    pub source_sha: String,
    pub source_url: String,
    pub media_type: String,
    pub canonicalization: String,
    pub sha256: String,
    /// Exact source bytes are independently reopened from this CAS object.
    pub storage_ref: String,
    pub byte_length: u64,
    pub bytes_base64: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0PullRequestInventory {
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub author_association: String,
    pub head_repository: String,
    pub head_sha: String,
    pub base_sha: String,
    pub tested_merge_sha: String,
    pub merge_group_sha: Option<String>,
    pub trust: G0TrustObservation,
    pub applicability: String,
    pub source_url: String,
    pub workflow_bindings: Vec<G0WorkflowBinding>,
    pub required_check_producers: Vec<G0CheckProducer>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0TrustObservation {
    pub state: String,
    pub reason: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0WorkflowBinding {
    pub workflow_path: String,
    pub workflow_revision: String,
    pub event: String,
    pub source_sha: String,
    pub actual_checkout_sha: String,
    pub run_ids: Vec<u64>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0CheckProducer {
    pub context: String,
    pub app_id: String,
    pub app_slug: String,
    /// Tagged evidence shape derived from captured provider API objects. The
    /// checker never invents Actions run/job identities for an external app.
    pub provider: G0CheckProvider,
    pub api: G0ApiKind,
    pub check_suite_id: u64,
    pub check_run_id: u64,
    pub source_sha: String,
    pub event: String,
    pub status: String,
    pub conclusion: String,
    /// The exact provider `html_url` returned by the captured check-run API
    /// object. `details_url` is a different field and is never accepted here.
    pub html_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum G0CheckProvider {
    GithubActions {
        workflow_run_id: u64,
        run_attempt: u32,
        job_id: u64,
        job_run_id: u64,
        job_run_attempt: u32,
        job_check_run_id: u64,
        job_source_sha: String,
        job_html_url: String,
        /// Checkout identity comes from the independently captured Actions
        /// execution, not from an external App check.
        actual_checkout_sha: String,
    },
    ExternalApp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RevisionReconciliation {
    pub pre_state: Vec<G0RepositoryRevision>,
    pub post_state: Vec<G0RepositoryRevision>,
    pub changed_refs: Vec<String>,
    pub invalidated: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0RepositoryRevision {
    pub repository: String,
    pub default_branch_sha: String,
    pub prs: Vec<G0PullRequestRevision>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0PullRequestRevision {
    pub number: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub tested_merge_sha: String,
    pub merge_group_sha: Option<String>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0DependencyGraph {
    pub nodes: Vec<G0GraphNode>,
    pub edges: Vec<G0GraphEdge>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0GraphNode {
    pub id: String,
    pub kind: String,
    pub repository: String,
    pub workload_id: String,
    pub applicability: String,
    pub source_sha: String,
    pub source_ref: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub required: bool,
    pub source_sha: String,
    pub source_ref: String,
    pub target_source_sha: String,
    pub target_source_ref: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0ModelSession {
    pub session_id: String,
    pub effective: bool,
    pub orchestrator_model: String,
    pub orchestrator_effort: String,
    pub agents: Vec<G0AgentModel>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0AgentModel {
    pub agent_id: String,
    pub model: String,
    pub effort: String,
    pub effective: bool,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0AccessObservation {
    pub repository: String,
    pub state: String,
    pub scopes: Vec<String>,
    pub gaps: Vec<String>,
    pub raw_object_refs: Vec<String>,
}
