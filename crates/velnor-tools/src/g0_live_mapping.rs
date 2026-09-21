//! Strict mapping from the live GitHub observation ledger to the checker G0
//! contract.  This adapter is deliberately fail-closed: optional provider
//! fields, unresolved action revisions, and missing rate/request identities
//! are errors, never synthetic IDs or successful empty rows.  External checks
//! remain typed observations and are never assigned synthetic Actions run/job
//! identities.

use super::live_collector::{
    LiveCheck, LiveCheckoutObservation, LiveCollection, LiveDependency, LiveExecution,
    LivePullRequest, LivePullRequestIdentity, LiveRepository, LiveWorkflow,
};
use super::{sha256_digest, AcquisitionState, ApiKind, HttpMethod, RawObjectRef, RequestRecord};
use crate::evidence_check::{ManifestDocument, ManifestRepository};
use crate::g0_contract::*;
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::collections::{BTreeMap, BTreeSet};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const ORCHESTRATOR_MODEL: &str = "gpt-6-astra";
const ORCHESTRATOR_EFFORT: &str = "low";
const AGENT_MODEL: &str = "gpt-5.6-luna";
const AGENT_EFFORT: &str = "max";
const GITHUB_API_VERSION: &str = "2026-03-10";

/// Inputs that cannot be safely invented by a GitHub API collector.  The
/// caller must provide the effective model session and the reviewed workload
/// artifact as separately captured, raw-bound values.
#[derive(Debug, Clone)]
pub struct G0MappingBindings {
    pub collector_name: String,
    pub collector_revision: String,
    pub phase: String,
    pub model_session: CapturedModelSession,
    pub workload_artifact: CapturedWorkloadArtifact,
    /// External CAS reference written by the same collector process for the
    /// canonical typed snapshot bytes.  A URI supplied without an object
    /// write is not accepted as authoritative evidence.
    pub collector_snapshot_storage_ref: String,
    /// Explicit workflow-to-workload mapping for repos with more than one
    /// reviewed workload.  A single-workload repo is mapped automatically.
    pub workflow_workloads: BTreeMap<(String, String), Vec<String>>,
}

/// Model-session evidence parsed from one collector-owned raw object.  The
/// fields stay private so a caller cannot construct an effective Luna/max
/// claim by filling the typed contract directly.
#[derive(Debug, Clone)]
pub struct CapturedModelSession {
    value: G0ModelSession,
    raw_object_ref: String,
}

impl CapturedModelSession {
    pub(crate) fn from_raw_object(raw: &RawObjectRef) -> Result<Self> {
        if raw.object_kind != "model.session" {
            bail!(
                "model session must come from a model.session raw object, got {}",
                raw.object_kind
            );
        }
        let value = parse_bound_json::<G0ModelSession>(raw, "model session")?;
        require_raw_binding(&value.raw_object_refs, raw, "model session")?;
        Ok(Self {
            value,
            raw_object_ref: raw.raw_id.clone(),
        })
    }

    fn value(&self) -> &G0ModelSession {
        &self.value
    }
}

/// Reviewed workload artifact evidence parsed from one collector-owned raw
/// object.  It is intentionally distinct from a caller-provided path or
/// digest string.
#[derive(Debug, Clone)]
pub struct CapturedWorkloadArtifact {
    value: G0ArtifactReference,
    raw_object_ref: String,
}

impl CapturedWorkloadArtifact {
    pub(crate) fn from_raw_object(raw: &RawObjectRef) -> Result<Self> {
        if raw.object_kind != "workload.artifact" {
            bail!(
                "workload artifact must come from a workload.artifact raw object, got {}",
                raw.object_kind
            );
        }
        let value = parse_bound_json::<G0ArtifactReference>(raw, "workload artifact")?;
        require_raw_binding(&value.raw_object_refs, raw, "workload artifact")?;
        Ok(Self {
            value,
            raw_object_ref: raw.raw_id.clone(),
        })
    }

    fn value(&self) -> &G0ArtifactReference {
        &self.value
    }
}

fn parse_bound_json<T: for<'de> serde::Deserialize<'de>>(
    raw: &RawObjectRef,
    label: &str,
) -> Result<T> {
    let bytes = BASE64
        .decode(&raw.bytes_base64)
        .with_context(|| format!("decode {label} raw bytes"))?;
    if bytes.len() as u64 != raw.byte_length || sha256_digest(&bytes) != raw.sha256 {
        bail!("{label} raw bytes do not match collector digest/length");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("parse bound {label} JSON"))
}

fn require_raw_binding(raw_ids: &[String], raw: &RawObjectRef, label: &str) -> Result<()> {
    if !raw_ids.iter().any(|raw_id| raw_id == &raw.raw_id) {
        bail!("{label} does not bind its source raw object {}", raw.raw_id);
    }
    Ok(())
}

/// Fields already captured by the live collector but absent from exact
/// checker `a613e041` types. Keeping this sidecar explicit prevents the
/// adapter from silently discarding original-response digests, canonical
/// request bytes, run-scoped artifact observations, checkout SHAs, or graph
/// source revisions while the checker owner evolves the typed contract.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G0MappingSupplement {
    pub canonical_snapshot_bytes_base64: String,
    pub request_payloads: BTreeMap<String, G0RequestPayloadSupplement>,
    pub raw_objects: Vec<G0RawObjectSupplement>,
    pub artifacts: Vec<G0ArtifactObservationSupplement>,
    pub checkout_observations: Vec<G0CheckoutObservationSupplement>,
    pub workflow_binding_checkout_shas: BTreeMap<String, String>,
    pub check_checkout_shas: BTreeMap<String, String>,
    pub pull_request_source_urls: BTreeMap<String, String>,
    pub graph_source: BTreeMap<String, (String, String)>,
    pub source_api_urls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G0RequestPayloadSupplement {
    pub query_base64: String,
    pub variables_base64: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G0RawObjectSupplement {
    pub raw_id: String,
    pub original_sha256: String,
    pub original_byte_length: u64,
    pub original_storage_ref: String,
    pub safe_sha256: String,
    pub safe_byte_length: u64,
    pub safe_bytes_base64: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G0ArtifactObservationSupplement {
    pub repository: String,
    pub artifact_id: u64,
    pub run_id: u64,
    pub run_attempt: Option<u32>,
    pub run_head_sha: String,
    pub name: String,
    pub digest: String,
    pub expired: Option<bool>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G0CheckoutObservationSupplement {
    pub subject: String,
    pub api_head_sha: String,
    pub checkout_sha: Option<String>,
    pub api_raw_object_refs: Vec<String>,
    pub proof_raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct G0MappedInventory {
    pub evidence: G0InventoryEvidence,
    pub supplement: G0MappingSupplement,
}

impl G0MappingBindings {
    pub fn validate(&self) -> Result<()> {
        if self.collector_name.trim().is_empty()
            || self.collector_revision.trim().is_empty()
            || self.phase.trim().is_empty()
        {
            bail!("G0 collector identity fields are required");
        }
        let session = self.model_session.value();
        if !session.effective
            || session.orchestrator_model != ORCHESTRATOR_MODEL
            || session.orchestrator_effort != ORCHESTRATOR_EFFORT
            || session.agents.is_empty()
            || session.agents.iter().any(|agent| {
                !agent.effective || agent.model != AGENT_MODEL || agent.effort != AGENT_EFFORT
            })
        {
            bail!("effective model session must be Astra/low with Luna/max agents");
        }
        if session.raw_object_refs.is_empty()
            || self.workload_artifact.value().raw_object_refs.is_empty()
            || self.model_session.raw_object_ref.trim().is_empty()
            || self.workload_artifact.raw_object_ref.trim().is_empty()
        {
            bail!("model and workload inputs require independently captured raw references");
        }
        Ok(())
    }
}

/// Produce the exact current typed G0 envelope plus a separately retained
/// canonical byte copy.  The checker may still reject this envelope for
/// contract gaps (for example intermediate page `complete=false` records);
/// this function preserves those facts instead of normalizing them away.
pub fn map_g0_inventory(
    live: &LiveCollection,
    manifest: &ManifestDocument,
    bindings: &G0MappingBindings,
) -> Result<G0InventoryEvidence> {
    Ok(map_g0_inventory_with_supplement(live, manifest, bindings)?.evidence)
}

pub fn map_g0_inventory_with_supplement(
    live: &LiveCollection,
    manifest: &ManifestDocument,
    bindings: &G0MappingBindings,
) -> Result<G0MappedInventory> {
    bindings.validate()?;
    if manifest.repositories.len() != 32 || live.repositories.len() != 32 {
        bail!("G0 mapping requires exactly 32 manifest and live repositories");
    }
    let raw_by_id = live
        .raw_objects
        .iter()
        .map(|raw| (raw.raw_id.clone(), raw))
        .collect::<BTreeMap<_, _>>();
    let request_by_id = live
        .requests
        .iter()
        .map(|request| (request.request_id.clone(), request))
        .collect::<BTreeMap<_, _>>();
    let requests = live
        .requests
        .iter()
        .map(map_request)
        .collect::<Result<Vec<_>>>()?;
    let raw_objects = live
        .raw_objects
        .iter()
        .map(map_raw_object)
        .collect::<Result<Vec<_>>>()?;
    let rate_limit = map_rate_limit(live)?;
    let repositories = live
        .repositories
        .iter()
        .map(|repository| {
            let manifest_repository = manifest
                .repositories
                .iter()
                .find(|candidate| candidate.repository == repository.repository)
                .ok_or_else(|| {
                    anyhow!(
                        "live repository {} absent from manifest",
                        repository.repository
                    )
                })?;
            map_repository(
                repository,
                manifest_repository,
                &raw_by_id,
                &request_by_id,
                &live.observed_at_utc,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let dependency_graph =
        map_dependency_graph(live, manifest, bindings, &raw_by_id, &request_by_id)?;
    let reconciliation = map_reconciliation(live, &raw_by_id)?;
    let auth = map_auth(live)?;
    let collector = G0CollectorIdentity {
        name: bindings.collector_name.clone(),
        revision: bindings.collector_revision.clone(),
        mode: "read-only-live".to_owned(),
        api_base: "https://api.github.com".to_owned(),
        api_versions: vec![GITHUB_API_VERSION.to_owned()],
    };
    let snapshot = G0CollectorSnapshot {
        schema_version: 1,
        snapshot_id: live.snapshot_id.clone(),
        manifest_id: live.manifest_id.clone(),
        phase: bindings.phase.clone(),
        observed_at_utc: live.observed_at_utc.clone(),
        completed_at_utc: live.completed_at_utc.clone(),
        collector,
        auth,
        rate_limit,
        requests,
        raw_objects,
        repositories,
        reconciliation,
        dependency_graph,
        model_session: bindings.model_session.value().clone(),
        access: map_access(live, &raw_by_id)?,
        workload_artifact: bindings.workload_artifact.value().clone(),
    };
    let canonical = canonical_json_bytes(&snapshot).context("serialize canonical G0 snapshot")?;
    let snapshot_sha256 = sha256_digest(&canonical);
    if bindings.collector_snapshot_storage_ref != canonical_storage_ref(&snapshot_sha256) {
        bail!("collector snapshot storage reference does not match canonical bytes");
    }
    let evidence = G0InventoryEvidence {
        collector_snapshot: snapshot,
        collector_snapshot_bytes_base64: BASE64.encode(&canonical),
        collector_snapshot_sha256: snapshot_sha256,
        collector_snapshot_storage_ref: bindings.collector_snapshot_storage_ref.clone(),
    };
    let supplement = map_supplement(live, &canonical);
    Ok(G0MappedInventory {
        evidence,
        supplement,
    })
}

fn map_auth(live: &LiveCollection) -> Result<G0AuthIdentity> {
    let viewer_id = live
        .auth
        .viewer_id
        .clone()
        .ok_or_else(|| anyhow!("live auth viewer ID is missing"))?;
    let viewer_login = live
        .auth
        .viewer_login
        .clone()
        .ok_or_else(|| anyhow!("live auth viewer login is missing"))?;
    if live.auth.provider != "github"
        || live.auth.safe_scopes.is_empty()
        || !live.auth.secret_excluded
    {
        bail!("live auth identity lacks provider, safe scopes, or secret exclusion");
    }
    Ok(G0AuthIdentity {
        provider: live.auth.provider.clone(),
        viewer_id,
        viewer_login,
        safe_scopes: live.auth.safe_scopes.iter().cloned().collect(),
        secret_excluded: true,
    })
}

fn map_rate_limit(live: &LiveCollection) -> Result<G0RateLimitObservation> {
    let observation = live
        .requests
        .iter()
        .rev()
        .find_map(|request| request.rate_limit.as_ref())
        .ok_or_else(|| anyhow!("live collection has no rate-limit headers"))?;
    let limit = observation
        .limit
        .ok_or_else(|| anyhow!("rate-limit limit missing"))?;
    let remaining = observation
        .remaining
        .ok_or_else(|| anyhow!("rate-limit remaining missing"))?;
    let used = observation
        .used
        .ok_or_else(|| anyhow!("rate-limit used missing"))?;
    let reset = observation
        .reset_at
        .as_deref()
        .ok_or_else(|| anyhow!("rate-limit reset missing"))?
        .parse::<i64>()
        .context("parse GitHub rate-limit reset")?;
    let reset_at_utc = OffsetDateTime::from_unix_timestamp(reset)
        .context("format GitHub rate-limit reset")?
        .format(&Rfc3339)
        .context("format rate-limit timestamp")?;
    Ok(G0RateLimitObservation {
        api: "core".to_owned(),
        limit,
        remaining,
        used,
        reset_at_utc,
        observed_at_utc: live.completed_at_utc.clone(),
    })
}

fn map_request(request: &RequestRecord) -> Result<G0RequestRecord> {
    let status = request
        .http_status
        .ok_or_else(|| anyhow!("request {} lacks HTTP status", request.request_id))?;
    let api_request_id = request
        .api_request_id
        .clone()
        .ok_or_else(|| anyhow!("request {} lacks API request ID", request.request_id))?;
    let rate_limit_ref = request
        .rate_limit
        .as_ref()
        .map(|_| "collector.rate_limit".to_owned())
        .ok_or_else(|| anyhow!("request {} lacks rate-limit provenance", request.request_id))?;
    let page = G0Page {
        number: request.page.number,
        per_page: request
            .page
            .per_page
            .ok_or_else(|| anyhow!("request {} lacks page size", request.request_id))?,
        link_next: request.page.link_next.clone(),
        cursor_in: request.page.cursor_in.clone(),
        cursor_out: request.page.cursor_out.clone(),
        has_next_page: request
            .page
            .has_next_page
            .ok_or_else(|| anyhow!("request {} lacks page terminal state", request.request_id))?,
        items_returned: request.page.items_returned as u32,
    };
    Ok(G0RequestRecord {
        request_id: request.request_id.clone(),
        api: match request.api {
            ApiKind::Rest => G0ApiKind::Rest,
            ApiKind::GraphQl => G0ApiKind::Graphql,
        },
        method: match request.method {
            HttpMethod::Get => "GET".to_owned(),
            HttpMethod::Post => "POST".to_owned(),
        },
        endpoint_or_operation: request.endpoint_or_operation.clone(),
        query_base64: request.query_base64.clone(),
        variables_base64: request.variables_base64.clone(),
        query_sha256: request
            .query_sha256
            .clone()
            .ok_or_else(|| anyhow!("request {} lacks query digest", request.request_id))?,
        variables_sha256: request
            .variables_sha256
            .clone()
            .ok_or_else(|| anyhow!("request {} lacks variables digest", request.request_id))?,
        auth_identity_ref: request.auth_identity_ref.clone(),
        started_at_utc: request.started_at_utc.clone(),
        completed_at_utc: request.completed_at_utc.clone(),
        http_status: status,
        api_request_id,
        rate_limit_ref,
        page,
        response_raw_ref: request
            .response_raw_ref
            .clone()
            .ok_or_else(|| anyhow!("request {} lacks raw response", request.request_id))?,
        error_raw_ref: request.error_raw_ref.clone(),
        state: map_request_state(request.state)?,
        complete: request.complete,
        truncation_reason: request.truncation_reason.clone(),
    })
}

fn map_request_state(state: AcquisitionState) -> Result<G0RequestState> {
    Ok(match state {
        AcquisitionState::Complete => G0RequestState::Complete,
        AcquisitionState::EmptyComplete => G0RequestState::EmptyComplete,
        AcquisitionState::Forbidden => G0RequestState::Forbidden,
        AcquisitionState::NotFound => G0RequestState::NotFound,
        AcquisitionState::RateLimited => G0RequestState::RateLimited,
        AcquisitionState::TransportError => G0RequestState::TransportError,
        AcquisitionState::Malformed => G0RequestState::Malformed,
        AcquisitionState::GraphQlError => G0RequestState::Malformed,
        AcquisitionState::DuplicateCursor | AcquisitionState::Truncated => {
            G0RequestState::Truncated
        }
        AcquisitionState::Unknown => G0RequestState::Unknown,
    })
}

fn map_raw_object(raw: &RawObjectRef) -> Result<G0RawObjectRef> {
    if raw.bytes_base64.trim().is_empty() {
        bail!("raw object {} lacks safe bytes", raw.raw_id);
    }
    let bytes = BASE64
        .decode(&raw.bytes_base64)
        .with_context(|| format!("decode safe bytes for {}", raw.raw_id))?;
    if bytes.len() as u64 != raw.byte_length || sha256_digest(&bytes) != raw.sha256 {
        bail!(
            "raw object {} safe bytes do not match digest/length",
            raw.raw_id
        );
    }
    if !is_sha256_digest(&raw.sha256)
        || !is_sha256_digest(&raw.original_sha256)
        || raw.storage_ref != canonical_storage_ref(&raw.sha256)
        || raw.original_storage_ref != canonical_storage_ref(&raw.original_sha256)
    {
        bail!(
            "raw object {} has a non-canonical storage binding",
            raw.raw_id
        );
    }
    Ok(G0RawObjectRef {
        raw_id: raw.raw_id.clone(),
        request_id: raw.request_id.clone(),
        object_kind: raw.object_kind.clone(),
        canonicalization: raw.canonicalization.clone(),
        sha256: raw.sha256.clone(),
        byte_length: raw.byte_length,
        bytes_base64: raw.bytes_base64.clone(),
        media_type: raw.media_type.clone(),
        storage_ref: raw.storage_ref.clone(),
        original_sha256: raw.original_sha256.clone(),
        original_byte_length: raw.original_byte_length,
        original_storage_ref: raw.original_storage_ref.clone(),
    })
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn canonical_storage_ref(digest: &str) -> String {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    format!("sha256://{hex}")
}

fn canonical_json_bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).context("convert G0 snapshot to JSON")?;
    Ok(canonical_json(&value)?.into_bytes())
}

fn canonical_json(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::Null => Ok("null".to_owned()),
        serde_json::Value::Bool(value) => Ok(value.to_string()),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        serde_json::Value::String(value) => {
            serde_json::to_string(value).context("serialize canonical JSON string")
        }
        serde_json::Value::Array(values) => {
            let members = values
                .iter()
                .map(canonical_json)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", members.join(",")))
        }
        serde_json::Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let members = keys
                .into_iter()
                .map(|key| -> Result<String> {
                    let value = values
                        .get(key)
                        .context("canonical JSON key disappeared during traversal")?;
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key).context("serialize canonical JSON key")?,
                        canonical_json(value)?
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{{}}}", members.join(",")))
        }
    }
}

fn map_supplement(live: &LiveCollection, canonical: &[u8]) -> G0MappingSupplement {
    let request_payloads = live
        .requests
        .iter()
        .map(|request| {
            (
                request.request_id.clone(),
                G0RequestPayloadSupplement {
                    query_base64: request.query_base64.clone(),
                    variables_base64: request.variables_base64.clone(),
                },
            )
        })
        .collect();
    let artifacts = live
        .repositories
        .iter()
        .flat_map(|repository| {
            repository
                .artifacts
                .iter()
                .map(|artifact| G0ArtifactObservationSupplement {
                    repository: repository.repository.clone(),
                    artifact_id: artifact.artifact_id,
                    run_id: artifact.run_id,
                    run_attempt: artifact.run_attempt,
                    run_head_sha: artifact.run_head_sha.clone(),
                    name: artifact.name.clone(),
                    digest: artifact.digest.clone(),
                    expired: artifact.expired,
                    source_url: artifact.source_url.clone(),
                    raw_object_refs: artifact.raw_object_refs.clone(),
                })
        })
        .collect();
    let raw_objects = live
        .raw_objects
        .iter()
        .map(|raw| G0RawObjectSupplement {
            raw_id: raw.raw_id.clone(),
            original_sha256: raw.original_sha256.clone(),
            original_byte_length: raw.original_byte_length,
            original_storage_ref: raw.original_storage_ref.clone(),
            safe_sha256: raw.sha256.clone(),
            safe_byte_length: raw.byte_length,
            safe_bytes_base64: raw.bytes_base64.clone(),
        })
        .collect();
    let mut workflow_binding_checkout_shas = BTreeMap::new();
    let mut check_checkout_shas = BTreeMap::new();
    let mut checkout_observations = Vec::new();
    let mut pull_request_source_urls = BTreeMap::new();
    let mut graph_source = BTreeMap::new();
    let mut source_api_urls = BTreeMap::new();
    for repository in &live.repositories {
        for workflow in &repository.workflows {
            let workflow_key = format!("workflow:{}:{}", repository.repository, workflow.path);
            graph_source.insert(
                workflow_key.clone(),
                (workflow.source_sha.clone(), workflow.path.clone()),
            );
            source_api_urls.insert(workflow_key, workflow.source_url.clone());
            for dependency in workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
            {
                let dependency_key = format!(
                    "dependency:{}:{}@{}",
                    dependency.repository, dependency.path, dependency.revision
                );
                if let Some(source_url) = &dependency.source_url {
                    source_api_urls.insert(dependency_key, source_url.clone());
                }
            }
        }
        for pull_request in &repository.open_prs {
            let pr_key = format!("{}#{}", repository.repository, pull_request.identity.number);
            pull_request_source_urls
                .insert(pr_key.clone(), pull_request.identity.source_url.clone());
            for execution in &pull_request.executions {
                checkout_observations.push(map_checkout_observation(
                    format!(
                        "{pr_key}:run/{}/attempt/{}",
                        execution.run_id, execution.run_attempt
                    ),
                    &execution.checkout,
                ));
                for job in &execution.jobs {
                    checkout_observations.push(map_checkout_observation(
                        format!(
                            "{pr_key}:run/{}/attempt/{}/job/{}",
                            execution.run_id, execution.run_attempt, job.job_id
                        ),
                        &job.checkout,
                    ));
                }
            }
            for binding in &pull_request.workflow_bindings {
                if let Some(checkout) = binding.checkout.actual_checkout_sha() {
                    workflow_binding_checkout_shas.insert(
                        format!("{pr_key}:{}:{}", binding.workflow_path, binding.event),
                        checkout.to_owned(),
                    );
                }
            }
            for check in &pull_request.checks {
                checkout_observations.push(map_checkout_observation(
                    format!("{pr_key}:check/{}", check.check_run_id),
                    &check.checkout,
                ));
                if let Some(checkout) = check.checkout.actual_checkout_sha() {
                    check_checkout_shas.insert(
                        format!("{pr_key}:{}:{}", check.context, check.check_run_id),
                        checkout.to_owned(),
                    );
                }
            }
        }
        for execution in &repository.main_executions {
            checkout_observations.push(map_checkout_observation(
                format!(
                    "{}:main:run/{}/attempt/{}",
                    repository.repository, execution.run_id, execution.run_attempt
                ),
                &execution.checkout,
            ));
            for job in &execution.jobs {
                checkout_observations.push(map_checkout_observation(
                    format!(
                        "{}:main:run/{}/attempt/{}/job/{}",
                        repository.repository, execution.run_id, execution.run_attempt, job.job_id
                    ),
                    &job.checkout,
                ));
            }
        }
        for check in &repository.main_checks {
            checkout_observations.push(map_checkout_observation(
                format!(
                    "{}:main:check/{}",
                    repository.repository, check.check_run_id
                ),
                &check.checkout,
            ));
            if let Some(checkout) = check.checkout.actual_checkout_sha() {
                check_checkout_shas.insert(
                    format!(
                        "{}:{}:{}",
                        repository.repository, check.context, check.check_run_id
                    ),
                    checkout.to_owned(),
                );
            }
        }
    }
    checkout_observations.sort_by(|left, right| left.subject.cmp(&right.subject));
    G0MappingSupplement {
        canonical_snapshot_bytes_base64: BASE64.encode(canonical),
        request_payloads,
        raw_objects,
        artifacts,
        checkout_observations,
        workflow_binding_checkout_shas,
        check_checkout_shas,
        pull_request_source_urls,
        graph_source,
        source_api_urls,
    }
}

fn map_checkout_observation(
    subject: String,
    observation: &LiveCheckoutObservation,
) -> G0CheckoutObservationSupplement {
    G0CheckoutObservationSupplement {
        subject,
        api_head_sha: observation.api_head_sha.clone(),
        checkout_sha: observation.actual_checkout_sha().map(str::to_owned),
        api_raw_object_refs: observation.api_raw_object_refs.clone(),
        proof_raw_object_refs: observation
            .proof
            .as_ref()
            .map(|proof| proof.raw_object_refs().to_vec())
            .unwrap_or_default(),
    }
}

fn map_repository(
    repository: &LiveRepository,
    manifest: &ManifestRepository,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    observed_at_utc: &str,
) -> Result<G0RepositoryInventory> {
    if repository.repository_id == 0
        || repository.closing_repository_id == 0
        || repository.closing_default_branch.is_empty()
        || repository.closing_default_branch_sha.is_empty()
    {
        bail!(
            "repository {} lacks opening/closing head proof",
            repository.repository
        );
    }
    if repository.source_invalidated {
        bail!(
            "repository {} changed during live collection",
            repository.repository
        );
    }
    validate_raw_references(
        &repository.raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("repository {}", repository.repository),
        &[],
    )?;
    for artifact in &repository.artifacts {
        validate_raw_references(
            &artifact.raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!(
                "artifact {} run {} in {}",
                artifact.artifact_id, artifact.run_id, repository.repository
            ),
            &["workflow_artifacts"],
        )?;
        if artifact.run_id == 0
            || artifact.run_attempt.is_none()
            || !is_sha(&artifact.run_head_sha)
            || !is_sha256_digest(&artifact.digest)
            || artifact.expired.is_none()
        {
            bail!(
                "artifact {} in {} lacks immutable run identity or digest",
                artifact.artifact_id,
                repository.repository
            );
        }
    }
    let rulesets = repository
        .rulesets
        .iter()
        .map(|ruleset| -> Result<G0RulesetInventory> {
            if !ruleset.complete {
                bail!("ruleset {} is incomplete", ruleset.ruleset_id);
            }
            Ok(G0RulesetInventory {
                ruleset_id: ruleset.ruleset_id,
                name: ruleset.name.clone(),
                // The API object is captured in raw refs; this page is the
                // repository settings projection required by the checker.
                source_url: format!(
                    "https://github.com/{}/settings/rules",
                    repository.repository
                ),
                complete: ruleset.complete,
                required_checks: ruleset
                    .required_checks
                    .iter()
                    .map(|check| {
                        Ok(G0RequiredCheckPolicy {
                            context: check.context.clone(),
                            app_id: check.app_id.clone().ok_or_else(|| {
                                anyhow!(
                                    "ruleset {} check {} lacks app identity",
                                    check.ruleset_id,
                                    check.context
                                )
                            })?,
                            ruleset_id: check.ruleset_id,
                            raw_object_refs: check.raw_object_refs.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                raw_object_refs: ruleset.raw_object_refs.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let workflows = repository
        .workflows
        .iter()
        .map(|workflow| {
            map_workflow(
                workflow,
                repository,
                manifest,
                raw_by_id,
                request_by_id,
                observed_at_utc,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let open_prs = repository
        .open_prs
        .iter()
        .map(|pr| map_pull_request(pr, manifest, repository))
        .collect::<Result<Vec<_>>>()?;
    let main_checks = repository
        .main_checks
        .iter()
        .map(|check| {
            map_check(
                check,
                &repository.workflows,
                Some(&repository.main_executions),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let artifacts = repository
        .artifacts
        .iter()
        .map(|artifact| {
            Ok(G0ArtifactObservation {
                artifact_id: artifact.artifact_id,
                run_id: artifact.run_id,
                run_attempt: artifact.run_attempt.ok_or_else(|| {
                    anyhow!("artifact {} lacks attempt identity", artifact.artifact_id)
                })?,
                run_head_sha: artifact.run_head_sha.clone(),
                name: artifact.name.clone(),
                digest: artifact.digest.clone(),
                expired: artifact.expired.ok_or_else(|| {
                    anyhow!("artifact {} lacks expired state", artifact.artifact_id)
                })?,
                source_url: artifact.source_url.clone(),
                raw_object_refs: artifact.raw_object_refs.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(G0RepositoryInventory {
        repository: repository.repository.clone(),
        repository_id: repository.repository_id,
        default_branch: repository.default_branch.clone(),
        default_branch_sha: repository.default_branch_sha.clone(),
        rulesets,
        workflows,
        artifacts,
        open_prs,
        main_checks,
        raw_object_refs: repository.raw_object_refs.clone(),
    })
}

fn map_workflow(
    workflow: &LiveWorkflow,
    repository: &LiveRepository,
    manifest: &ManifestRepository,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    observed_at_utc: &str,
) -> Result<G0WorkflowInventory> {
    validate_raw_references(
        &workflow.raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("workflow {}", workflow.path),
        &["workflows", "workflow.source"],
    )?;
    validate_raw_references(
        &workflow.source_raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("workflow source {}", workflow.path),
        &["workflow.source"],
    )?;
    let source = map_source(
        &repository.repository,
        &workflow.path,
        &workflow.revision,
        &workflow.source_sha,
        &workflow.source_bytes_base64,
        &workflow.source_raw_object_refs,
        raw_by_id,
    )?;
    for dependency in workflow
        .reusable_workflows
        .iter()
        .chain(workflow.actions.iter())
        .chain(workflow.scanners.iter())
    {
        validate_raw_references(
            &dependency.raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!(
                "workflow dependency {}/{}@{}",
                dependency.repository, dependency.path, dependency.revision
            ),
            &["workflow.dependency"],
        )?;
        validate_raw_references(
            &dependency.source_raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!(
                "workflow dependency source {}/{}@{}",
                dependency.repository, dependency.path, dependency.revision
            ),
            &["workflow.dependency.source"],
        )?;
    }
    let generated_state = G0ArtifactReference {
        name: workflow.path.clone(),
        schema: "github.workflow-source.v1".to_owned(),
        source_url: source.source_url.clone(),
        sha256: source.sha256.clone(),
        storage_ref: source.storage_ref.clone(),
        source_revision: workflow.revision.clone(),
        source_digest: source.sha256.clone(),
        observed_at_utc: observed_at_utc.to_owned(),
        raw_object_refs: workflow.source_raw_object_refs.clone(),
    };
    let reviewed_workflow = workflow.path == manifest.workflow_path;
    let source_jobs = workflow
        .source_jobs
        .iter()
        .map(|job| {
            // Only the reviewed workflow path consults the expected-job
            // manifest here; auxiliary workflows emit their observed job IDs
            // with empty typed identity so a coincidental job-id match can
            // never inherit reviewed workload/provider/target metadata.
            // Typed identity stays enforced by the checker gate
            // check_g0_source_jobs (evidence_check.rs), which only runs for
            // the reviewed workflow path and revision.
            let expected = if reviewed_workflow {
                Some(
                    manifest
                        .expected_jobs
                        .iter()
                        .find(|expected| expected.job_id == job.job_id)
                        .ok_or_else(|| {
                            anyhow!(
                                "workflow source job {} is absent from reviewed manifest",
                                job.job_id
                            )
                        })?,
                )
            } else {
                None
            };
            validate_raw_references(
                &job.raw_object_refs,
                raw_by_id,
                request_by_id,
                &format!("workflow source job {}", job.job_id),
                &["workflow.source"],
            )?;
            let (workload_id, provider, platform, architecture, required) = expected.map_or_else(
                || {
                    (
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        false,
                    )
                },
                |expected| {
                    (
                        expected.workload_id.clone(),
                        expected.provider.clone(),
                        expected.platform.clone(),
                        expected.architecture.clone(),
                        expected.required,
                    )
                },
            );
            Ok(G0SourceJob {
                job_id: job.job_id.clone(),
                workload_id,
                provider,
                platform,
                architecture,
                required,
                raw_object_refs: job.raw_object_refs.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(G0WorkflowInventory {
        source,
        events: workflow.events.clone(),
        source_jobs,
        reusable_workflows: workflow
            .reusable_workflows
            .iter()
            .map(|dependency| map_dependency(dependency, raw_by_id))
            .collect::<Result<Vec<_>>>()?,
        actions: workflow
            .actions
            .iter()
            .map(|dependency| map_dependency(dependency, raw_by_id))
            .collect::<Result<Vec<_>>>()?,
        scanners: workflow
            .scanners
            .iter()
            .map(|dependency| map_dependency(dependency, raw_by_id))
            .collect::<Result<Vec<_>>>()?,
        generated_state,
        raw_object_refs: workflow.raw_object_refs.clone(),
    })
}

fn map_source(
    repository: &str,
    path: &str,
    revision: &str,
    source_sha: &str,
    bytes_base64: &str,
    raw_object_refs: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
) -> Result<G0WorkflowSource> {
    if path.trim().is_empty() || !is_sha(revision) || !is_sha(source_sha) {
        bail!("workflow source {} has malformed immutable identity", path);
    }
    let bytes = BASE64
        .decode(bytes_base64)
        .with_context(|| format!("decode workflow source {repository}/{path}"))?;
    if bytes.is_empty() {
        bail!("workflow source {repository}/{path} is empty");
    }
    let sha256 = sha256_digest(&bytes);
    let raw = raw_object_refs
        .iter()
        .filter_map(|raw_id| raw_by_id.get(raw_id).copied())
        .find(|raw| {
            raw.object_kind == "workflow.source"
                && raw.sha256 == sha256
                && raw.bytes_base64 == bytes_base64
        })
        .ok_or_else(|| {
            anyhow!(
                "workflow source {repository}/{path} lacks a raw object bound to exact source bytes"
            )
        })?;
    Ok(G0WorkflowSource {
        repository: repository.to_owned(),
        path: path.to_owned(),
        revision: revision.to_owned(),
        source_sha: source_sha.to_owned(),
        source_url: format!("https://github.com/{repository}/blob/{source_sha}/{path}"),
        media_type: "text/yaml".to_owned(),
        canonicalization: "raw-utf8".to_owned(),
        sha256,
        storage_ref: canonical_storage_ref(&raw.sha256),
        byte_length: bytes.len() as u64,
        bytes_base64: bytes_base64.to_owned(),
        raw_object_refs: raw_object_refs.to_owned(),
    })
}

fn map_dependency(
    dependency: &LiveDependency,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
) -> Result<G0WorkflowDependency> {
    if !is_sha(&dependency.revision) {
        bail!(
            "workflow dependency {}/{} remains tag/ref-bound at {}",
            dependency.repository,
            dependency.path,
            dependency.revision
        );
    }
    let path = dependency
        .resolved_path
        .as_deref()
        .unwrap_or(&dependency.path);
    let bytes_base64 = dependency
        .source_bytes_base64
        .as_deref()
        .ok_or_else(|| anyhow!("workflow dependency {path} lacks source bytes"))?;
    let source = map_source(
        &dependency.repository,
        path,
        &dependency.revision,
        &dependency.revision,
        bytes_base64,
        &dependency.source_raw_object_refs,
        raw_by_id,
    )?;
    Ok(G0WorkflowDependency {
        kind: dependency.kind.clone(),
        source,
    })
}

fn validate_raw_references(
    raw_ids: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    label: &str,
    allowed_kinds: &[&str],
) -> Result<()> {
    if raw_ids.is_empty() {
        bail!("{label} lacks raw object references");
    }
    for raw_id in raw_ids {
        let raw = raw_by_id
            .get(raw_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} references missing raw object {raw_id}"))?;
        if !allowed_kinds.is_empty() && !allowed_kinds.contains(&raw.object_kind.as_str()) {
            bail!(
                "{label} raw object {raw_id} has unexpected kind {}",
                raw.object_kind
            );
        }
        let request = request_by_id
            .get(&raw.request_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} raw object {raw_id} has missing request"))?;
        if !request.complete
            || !matches!(
                request.state,
                AcquisitionState::Complete | AcquisitionState::EmptyComplete
            )
        {
            bail!(
                "{label} raw object {raw_id} is bound to incomplete request {}",
                request.request_id
            );
        }
        if request.response_raw_ref.as_deref() != Some(raw_id.as_str()) {
            bail!(
                "{label} raw object {raw_id} is not the request response for {}",
                request.request_id
            );
        }
    }
    Ok(())
}

fn map_pull_request(
    pull_request: &LivePullRequest,
    manifest: &ManifestRepository,
    repository: &LiveRepository,
) -> Result<G0PullRequestInventory> {
    let identity = &pull_request.identity;
    let head_repository = identity
        .head_repository
        .clone()
        .ok_or_else(|| anyhow!("PR #{} has no head repository identity", identity.number))?;
    let tested_merge_sha = identity
        .tested_merge_sha
        .clone()
        .ok_or_else(|| anyhow!("PR #{} has no tested merge SHA", identity.number))?;
    let applicability = if manifest.expected_workload_ids.is_empty() {
        "not-applicable".to_owned()
    } else {
        "required".to_owned()
    };
    let trust = if head_repository == repository.repository {
        G0TrustObservation {
            state: "trusted".to_owned(),
            reason: "same-repository-head".to_owned(),
            raw_object_refs: identity.raw_object_refs.clone(),
        }
    } else {
        G0TrustObservation {
            state: "untrusted".to_owned(),
            reason: "fork-head".to_owned(),
            raw_object_refs: identity.raw_object_refs.clone(),
        }
    };
    let workflow_bindings = pull_request
        .workflow_bindings
        .iter()
        .map(|binding| {
            let _actual_checkout_sha = binding.checkout.actual_checkout_sha().ok_or_else(|| {
                anyhow!(
                    "PR #{} workflow binding lacks checkout SHA",
                    identity.number
                )
            })?;
            Ok(G0WorkflowBinding {
                workflow_path: binding.workflow_path.clone(),
                workflow_revision: binding.workflow_revision.clone(),
                event: binding.event.clone(),
                source_sha: binding.source_sha.clone(),
                actual_checkout_sha: binding
                    .checkout
                    .actual_checkout_sha()
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!(
                            "PR #{} workflow binding lacks checkout SHA",
                            identity.number
                        )
                    })?,
                run_ids: binding.run_ids.clone(),
                raw_object_refs: binding.raw_object_refs.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected = manifest
        .required_check_contexts_and_apps
        .iter()
        .map(|required| (required.context.clone(), required.app_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    for check in &pull_request.checks {
        let app_id = check
            .app_id
            .clone()
            .ok_or_else(|| anyhow!("check {} lacks provider App identity", check.context))?;
        let key = (check.context.clone(), app_id);
        if !expected.contains(&key) {
            bail!(
                "PR #{} contains unexpected check {}",
                identity.number,
                check.context
            );
        }
        if !observed.insert(key) {
            bail!("PR #{} repeats check {}", identity.number, check.context);
        }
    }
    if observed != expected {
        let missing = expected.difference(&observed).collect::<Vec<_>>();
        bail!(
            "PR #{} is missing required checks: {missing:?}",
            identity.number
        );
    }
    let required_check_producers = pull_request
        .checks
        .iter()
        .map(|check| map_check(check, &repository.workflows, Some(&pull_request.executions)))
        .collect::<Result<Vec<_>>>()?;
    Ok(G0PullRequestInventory {
        number: identity.number,
        state: identity.state.clone(),
        draft: identity.draft,
        author: identity.author.clone(),
        author_association: identity.author_association.clone(),
        head_repository,
        head_sha: identity.head_sha.clone(),
        base_sha: identity.base_sha.clone(),
        tested_merge_sha,
        merge_group_sha: identity.merge_group_sha.clone(),
        trust,
        applicability,
        source_url: identity.source_url.clone(),
        workflow_bindings,
        required_check_producers,
        raw_object_refs: pull_request.raw_object_refs.clone(),
    })
}

fn map_check(
    check: &LiveCheck,
    workflows: &[LiveWorkflow],
    executions: Option<&[LiveExecution]>,
) -> Result<G0CheckProducer> {
    let app_id = check
        .app_id
        .clone()
        .ok_or_else(|| anyhow!("check {} lacks provider App identity", check.context))?;
    if check.app_slug.trim().is_empty() {
        bail!("check {} lacks provider App slug", check.context);
    }
    let check_suite_id = check
        .check_suite_id
        .ok_or_else(|| anyhow!("check {} lacks suite identity", check.context))?;

    // External providers are represented as typed observations.  They must
    // retain their App/check URL identity, but must never be given synthetic
    // Actions run/job identities merely because the check suite happened to
    // contain a workflow run.
    if check.app_slug != "github-actions" {
        let event = check
            .event
            .clone()
            .ok_or_else(|| anyhow!("external check {} lacks event", check.context))?;
        return Ok(G0CheckProducer {
            context: check.context.clone(),
            app_id,
            app_slug: check.app_slug.clone(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id,
            check_run_id: check.check_run_id,
            source_sha: check.source_sha.clone(),
            event,
            status: check.status.clone(),
            conclusion: check.conclusion.clone().unwrap_or_default(),
            html_url: check.source_url.clone(),
            raw_object_refs: check.raw_object_refs.clone(),
        });
    }

    let workflow_run_id = check.workflow_run_id.ok_or_else(|| {
        anyhow!(
            "external check {} lacks workflow association",
            check.context
        )
    })?;
    let check_run_attempt = check
        .run_attempt
        .ok_or_else(|| anyhow!("check {} lacks run attempt", check.context))?;
    let matching_executions = executions
        .into_iter()
        .flatten()
        .filter(|run| run.run_id == workflow_run_id && run.run_attempt == check_run_attempt)
        .collect::<Vec<_>>();
    if matching_executions.len() != 1 {
        bail!(
            "check {} must bind exactly one workflow execution for run {workflow_run_id} attempt {check_run_attempt}, found {}",
            check.context,
            matching_executions.len()
        );
    }
    let execution = executions
        .and_then(|runs| {
            runs.iter()
                .find(|run| run.run_id == workflow_run_id && run.run_attempt == check_run_attempt)
        })
        .ok_or_else(|| anyhow!("check {} lacks concrete workflow execution", check.context))?;
    if execution.source_sha != check.source_sha {
        bail!(
            "check {} source SHA differs from workflow execution",
            check.context
        );
    }
    let workflow = workflows
        .iter()
        .find(|workflow| {
            workflow.path == execution.workflow_path
                && workflow.revision == execution.workflow_revision
                && workflow.source_sha == execution.source_sha
        })
        .ok_or_else(|| {
            anyhow!(
                "check {} workflow source/revision is not bound to inventory",
                check.context
            )
        })?;
    if !workflow.events.contains(&execution.event) {
        bail!(
            "check {} event {} is not declared by workflow {}",
            check.context,
            execution.event,
            workflow.path
        );
    }
    let matching_jobs = execution
        .jobs
        .iter()
        .filter(|job| job.check_run_id == check.check_run_id)
        .collect::<Vec<_>>();
    if matching_jobs.len() != 1 {
        bail!(
            "check {} must bind exactly one job for check {}, found {}",
            check.context,
            check.check_run_id,
            matching_jobs.len()
        );
    }
    let job = matching_jobs[0];
    if job.run_id != execution.run_id
        || job.run_attempt != execution.run_attempt
        || job.source_sha.as_deref() != Some(check.source_sha.as_str())
    {
        bail!(
            "check {} job identity is not bound to workflow execution",
            check.context
        );
    }
    let job_id = job.job_id;
    let actual_checkout_sha = check
        .checkout
        .actual_checkout_sha()
        .or_else(|| execution.checkout.actual_checkout_sha())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("check {} lacks checkout identity", check.context))?;
    let run_attempt = check_run_attempt;
    if run_attempt != execution.run_attempt {
        bail!(
            "check {} attempt differs from workflow execution",
            check.context
        );
    }
    let event = execution.event.clone();
    if let Some(check_event) = &check.event
        && check_event != &event
    {
        bail!(
            "check {} event differs from workflow execution",
            check.context
        );
    }
    Ok(G0CheckProducer {
        context: check.context.clone(),
        app_id,
        app_slug: check.app_slug.clone(),
        provider: G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            job_id,
            job_run_id: job.run_id,
            job_run_attempt: job.run_attempt,
            job_check_run_id: job.check_run_id,
            job_source_sha: job
                .source_sha
                .clone()
                .ok_or_else(|| anyhow!("check {} job lacks source SHA", check.context))?,
            job_html_url: job.source_url.clone(),
            actual_checkout_sha,
        },
        api: G0ApiKind::Rest,
        check_suite_id,
        check_run_id: check.check_run_id,
        source_sha: check.source_sha.clone(),
        event,
        status: check.status.clone(),
        conclusion: check.conclusion.clone().unwrap_or_else(|| "".to_owned()),
        html_url: check.source_url.clone(),
        raw_object_refs: check.raw_object_refs.clone(),
    })
}

fn map_access(
    live: &LiveCollection,
    _raw_by_id: &BTreeMap<String, &RawObjectRef>,
) -> Result<Vec<G0AccessObservation>> {
    Ok(live
        .repositories
        .iter()
        .map(|repository| G0AccessObservation {
            repository: repository.repository.clone(),
            state: if repository.access_state == "observed" && repository.access_gaps.is_empty() {
                "complete".to_owned()
            } else {
                "unknown".to_owned()
            },
            scopes: live.auth.safe_scopes.iter().cloned().collect(),
            gaps: repository.access_gaps.clone(),
            raw_object_refs: repository.raw_object_refs.clone(),
        })
        .collect())
}

fn map_dependency_graph(
    live: &LiveCollection,
    manifest: &ManifestDocument,
    bindings: &G0MappingBindings,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
) -> Result<G0DependencyGraph> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut graph_raw_ids = BTreeSet::new();
    for manifest_repository in &manifest.repositories {
        let repository = live
            .repositories
            .iter()
            .find(|repository| repository.repository == manifest_repository.repository)
            .ok_or_else(|| {
                anyhow!(
                    "{} absent from live collection",
                    manifest_repository.repository
                )
            })?;
        let workflow = repository
            .workflows
            .iter()
            .find(|workflow| {
                workflow.path == manifest_repository.workflow_path
                    && workflow.revision == manifest_repository.workflow_revision
            })
            .ok_or_else(|| {
                anyhow!(
                    "{} reviewed workflow {}@{} absent from live collection",
                    manifest_repository.repository,
                    manifest_repository.workflow_path,
                    manifest_repository.workflow_revision
                )
            })?;
        validate_raw_references(
            &workflow.raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!("dependency graph source {}", manifest_repository.repository),
            &["workflows", "workflow.source"],
        )?;
        let workloads = bindings
            .workflow_workloads
            .get(&(
                manifest_repository.repository.clone(),
                workflow.path.clone(),
            ))
            .cloned()
            .or_else(|| {
                (manifest_repository.expected_workload_ids.len() == 1)
                    .then(|| manifest_repository.expected_workload_ids.clone())
            })
            .ok_or_else(|| {
                anyhow!(
                    "{} requires explicit workflow-to-workload mapping",
                    manifest_repository.repository
                )
            })?;
        if workloads.is_empty() {
            bail!(
                "{} workflow maps to no workloads",
                manifest_repository.repository
            );
        }
        let source_raw_refs = workflow.source_raw_object_refs.clone();
        graph_raw_ids.extend(source_raw_refs.iter().cloned());
        for workload in workloads {
            if !manifest_repository
                .expected_workload_ids
                .contains(&workload)
            {
                bail!("workflow mapping names unreviewed workload {workload}");
            }
            let workload_node_id =
                format!("workload:{}:{}", manifest_repository.repository, workload);
            if !nodes
                .iter()
                .any(|node: &G0GraphNode| node.id == workload_node_id)
            {
                nodes.push(G0GraphNode {
                    id: workload_node_id.clone(),
                    kind: "workload".to_owned(),
                    repository: manifest_repository.repository.clone(),
                    workload_id: workload.clone(),
                    applicability: "required".to_owned(),
                    source_sha: workflow.source_sha.clone(),
                    source_ref: workflow.path.clone(),
                    raw_object_refs: source_raw_refs.clone(),
                });
            }
            for dependency in workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
            {
                validate_raw_references(
                    &dependency.raw_object_refs,
                    raw_by_id,
                    request_by_id,
                    &format!(
                        "dependency graph edge {}/{}@{}",
                        dependency.repository, dependency.path, dependency.revision
                    ),
                    &["workflow.dependency"],
                )?;
                validate_raw_references(
                    &dependency.source_raw_object_refs,
                    raw_by_id,
                    request_by_id,
                    &format!(
                        "dependency graph source {}/{}@{}",
                        dependency.repository, dependency.path, dependency.revision
                    ),
                    &["workflow.dependency.source"],
                )?;
                if !is_sha(&dependency.revision) {
                    bail!(
                        "dependency {}/{} has unresolved revision {}",
                        dependency.repository,
                        dependency.path,
                        dependency.revision
                    );
                }
                let dependency_node_id = format!(
                    "dependency:{}:{}@{}",
                    dependency.repository, dependency.path, dependency.revision
                );
                if !nodes
                    .iter()
                    .any(|node: &G0GraphNode| node.id == dependency_node_id)
                {
                    nodes.push(G0GraphNode {
                        id: dependency_node_id.clone(),
                        kind: dependency.kind.clone(),
                        repository: dependency.repository.clone(),
                        workload_id: workload.clone(),
                        applicability: "required".to_owned(),
                        source_sha: dependency.revision.clone(),
                        source_ref: dependency
                            .resolved_path
                            .clone()
                            .unwrap_or_else(|| dependency.path.clone()),
                        raw_object_refs: dependency.raw_object_refs.clone(),
                    });
                }
                graph_raw_ids.extend(dependency.raw_object_refs.iter().cloned());
                edges.push(G0GraphEdge {
                    from: workload_node_id.clone(),
                    to: dependency_node_id,
                    kind: dependency.kind.clone(),
                    required: true,
                    source_sha: workflow.source_sha.clone(),
                    source_ref: workflow.path.clone(),
                    target_source_sha: dependency.revision.clone(),
                    target_source_ref: dependency
                        .resolved_path
                        .clone()
                        .unwrap_or_else(|| dependency.path.clone()),
                    raw_object_refs: dependency.raw_object_refs.clone(),
                });
            }
        }
    }
    if nodes.is_empty() || edges.is_empty() {
        bail!("live workflow graph has no source-bound dependency edges");
    }
    Ok(G0DependencyGraph {
        nodes,
        edges,
        raw_object_refs: graph_raw_ids.into_iter().collect(),
    })
}

fn map_reconciliation(
    live: &LiveCollection,
    _raw_by_id: &BTreeMap<String, &RawObjectRef>,
) -> Result<G0RevisionReconciliation> {
    let pre_state = map_revision_state(live, &live.opening_prs)?;
    let post_state = map_revision_state(live, &live.closing_prs)?;
    let changed_refs = live
        .reconciliation
        .changes
        .iter()
        .map(|change| change.key.clone())
        .collect::<Vec<_>>();
    Ok(G0RevisionReconciliation {
        pre_state,
        post_state,
        changed_refs: changed_refs.clone(),
        invalidated: if changed_refs.is_empty() {
            Vec::new()
        } else {
            changed_refs
        },
    })
}

fn map_revision_state(
    live: &LiveCollection,
    identities: &[LivePullRequestIdentity],
) -> Result<Vec<G0RepositoryRevision>> {
    let mut by_repository = BTreeMap::<String, Vec<&LivePullRequestIdentity>>::new();
    for identity in identities {
        let repository = identity
            .base_repository
            .clone()
            .ok_or_else(|| anyhow!("PR #{} lacks base repository", identity.number))?;
        by_repository.entry(repository).or_default().push(identity);
    }
    live.repositories
        .iter()
        .map(|repository| {
            let prs = by_repository
                .remove(&repository.repository)
                .unwrap_or_default()
                .into_iter()
                .map(|identity| {
                    let tested_merge_sha = identity
                        .tested_merge_sha
                        .clone()
                        .ok_or_else(|| anyhow!("PR #{} lacks tested merge SHA", identity.number))?;
                    Ok(G0PullRequestRevision {
                        number: identity.number,
                        head_sha: identity.head_sha.clone(),
                        base_sha: identity.base_sha.clone(),
                        tested_merge_sha,
                        merge_group_sha: identity.merge_group_sha.clone(),
                        raw_object_refs: identity.raw_object_refs.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(G0RepositoryRevision {
                repository: repository.repository.clone(),
                default_branch_sha: repository.default_branch_sha.clone(),
                prs,
                raw_object_refs: repository.raw_object_refs.clone(),
            })
        })
        .collect()
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn utc_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::github_acquisition::sha256_digest;
    use base64::engine::general_purpose::STANDARD as BASE64;

    fn raw_object(kind: &str, raw_id: &str, value: serde_json::Value) -> RawObjectRef {
        let bytes = serde_json::to_vec(&value).expect("fixture JSON");
        let digest = sha256_digest(&bytes);
        let digest_hex = digest.strip_prefix("sha256:").expect("digest prefix");
        RawObjectRef {
            raw_id: raw_id.to_owned(),
            request_id: format!("{raw_id}-request"),
            object_kind: kind.to_owned(),
            canonicalization: "json-utf8".to_owned(),
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            bytes_base64: BASE64.encode(&bytes),
            media_type: "application/json".to_owned(),
            storage_ref: format!("sha256://{digest_hex}"),
            original_sha256: digest.clone(),
            original_byte_length: bytes.len() as u64,
            original_storage_ref: format!("sha256://{digest_hex}"),
        }
    }

    #[test]
    fn model_and_workload_bindings_require_typed_raw_objects() {
        let model = raw_object(
            "model.session",
            "model-1",
            serde_json::json!({
                "session_id": "session-1",
                "effective": true,
                "orchestrator_model": "gpt-6-astra",
                "orchestrator_effort": "low",
                "agents": [{
                    "agent_id": "agent-1",
                    "model": "gpt-5.6-luna",
                    "effort": "max",
                    "effective": true,
                    "raw_object_refs": ["model-1"]
                }],
                "raw_object_refs": ["model-1"]
            }),
        );
        let workload = raw_object(
            "workload.artifact",
            "workload-1",
            serde_json::json!({
                "name": "fleet.json",
                "schema": "github-first-dual-lane.v2",
                "source_url": "https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/docs/ci/github-first-dual-lane/fleet.json",
                "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "storage_ref": "sha256://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "source_revision": "abe9ad82a2d4d01b706bbc6122ab6ccb150faad9",
                "source_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "observed_at_utc": "2026-09-20T00:00:00Z",
                "raw_object_refs": ["workload-1"]
            }),
        );
        let captured_model =
            CapturedModelSession::from_raw_object(&model).expect("typed model binding");
        let captured_workload =
            CapturedWorkloadArtifact::from_raw_object(&workload).expect("typed workload binding");
        assert_eq!(captured_model.raw_object_ref, "model-1");
        assert_eq!(captured_workload.raw_object_ref, "workload-1");
        assert_eq!(captured_model.value.session_id, "session-1");
        assert_eq!(captured_workload.value.name, "fleet.json");

        let mut wrong_kind = model.clone();
        wrong_kind.object_kind = "caller.claim".to_owned();
        assert!(CapturedModelSession::from_raw_object(&wrong_kind).is_err());
    }

    #[test]
    fn source_jobs_enforce_manifest_only_for_reviewed_workflow_path() {
        use crate::evidence_check::ExpectedJobSpec;
        use crate::github_acquisition::live_collector::LiveSourceJob;
        use crate::github_acquisition::PageState;

        fn complete_request(raw_id: &str) -> RequestRecord {
            RequestRecord {
                request_id: format!("{raw_id}-request"),
                api: ApiKind::Rest,
                method: HttpMethod::Get,
                endpoint_or_operation: "https://api.github.com/repos/tailrocks/example".to_owned(),
                query_base64: String::new(),
                variables_base64: String::new(),
                query_sha256: None,
                variables_sha256: None,
                redacted_variables: None,
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: None,
                api_request_id: None,
                rate_limit: None,
                safe_scopes: None,
                page: PageState {
                    number: 1,
                    per_page: Some(100),
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: 0,
                },
                response_raw_ref: Some(raw_id.to_owned()),
                error_raw_ref: None,
                state: AcquisitionState::Complete,
                complete: true,
                truncation_reason: None,
            }
        }

        let source_bytes = b"jobs:\n  release:\n    steps: []\n";
        let source_base64 = BASE64.encode(source_bytes);
        let digest = sha256_digest(source_bytes);
        let digest_hex = digest.strip_prefix("sha256:").expect("digest prefix");
        let raw_src = RawObjectRef {
            raw_id: "raw-src".to_owned(),
            request_id: "raw-src-request".to_owned(),
            object_kind: "workflow.source".to_owned(),
            canonicalization: "raw-utf8".to_owned(),
            sha256: digest.clone(),
            byte_length: source_bytes.len() as u64,
            original_sha256: digest.clone(),
            original_byte_length: source_bytes.len() as u64,
            bytes_base64: source_base64.clone(),
            media_type: "text/yaml".to_owned(),
            storage_ref: format!("sha256://{digest_hex}"),
            original_storage_ref: format!("sha256://{digest_hex}"),
        };
        let raw_wf = raw_object("workflows", "raw-wf", serde_json::json!({"path": "x"}));
        let raw_by_id: BTreeMap<String, &RawObjectRef> = [
            ("raw-src".to_owned(), &raw_src),
            ("raw-wf".to_owned(), &raw_wf),
        ]
        .into_iter()
        .collect();
        let request_src = complete_request("raw-src");
        let request_wf = complete_request("raw-wf");
        let request_by_id: BTreeMap<String, &RequestRecord> = [
            ("raw-src-request".to_owned(), &request_src),
            ("raw-wf-request".to_owned(), &request_wf),
        ]
        .into_iter()
        .collect();
        let repository = LiveRepository {
            repository: "tailrocks/example".to_owned(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: "0123456789012345678901234567890123456789".to_owned(),
            rulesets: Vec::new(),
            workflows: Vec::new(),
            open_prs: Vec::new(),
            artifacts: Vec::new(),
            main_executions: Vec::new(),
            main_checks: Vec::new(),
            closing_repository_id: 1,
            closing_default_branch: "main".to_owned(),
            closing_default_branch_sha: "0123456789012345678901234567890123456789".to_owned(),
            source_invalidated: false,
            access_state: "ok".to_owned(),
            access_gaps: Vec::new(),
            raw_object_refs: Vec::new(),
        };
        let manifest = ManifestRepository {
            repository: "tailrocks/example".to_owned(),
            workflow_path: ".github/workflows/ci.yml".to_owned(),
            expected_jobs: vec![ExpectedJobSpec {
                job_id: "unit".to_owned(),
                workload_id: "tailrocks/example:unit".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "x64".to_owned(),
                required: true,
                child_workflow: None,
            }],
            ..ManifestRepository::default()
        };
        let workflow = LiveWorkflow {
            path: ".github/workflows/release.yml".to_owned(),
            revision: "0123456789012345678901234567890123456789".to_owned(),
            source_sha: "0123456789012345678901234567890123456789".to_owned(),
            source_url: "https://api.github.com/repos/tailrocks/example/contents/x".to_owned(),
            source_bytes_base64: source_base64,
            source_raw_object_refs: vec!["raw-src".to_owned()],
            events: vec!["push".to_owned()],
            source_jobs: vec![LiveSourceJob {
                job_id: "release".to_owned(),
                raw_object_refs: vec!["raw-src".to_owned()],
            }],
            reusable_workflows: Vec::new(),
            actions: Vec::new(),
            scanners: Vec::new(),
            raw_object_refs: vec!["raw-wf".to_owned()],
        };

        let mapped = map_workflow(
            &workflow,
            &repository,
            &manifest,
            &raw_by_id,
            &request_by_id,
            "2026-09-20T00:00:00Z",
        )
        .expect("auxiliary workflow keeps observed job IDs");
        assert_eq!(mapped.source_jobs.len(), 1);
        assert_eq!(mapped.source_jobs[0].job_id, "release");
        assert_eq!(mapped.source_jobs[0].raw_object_refs, vec!["raw-src"]);
        assert!(mapped.source_jobs[0].workload_id.is_empty());
        assert!(mapped.source_jobs[0].provider.is_empty());
        assert!(mapped.source_jobs[0].platform.is_empty());
        assert!(mapped.source_jobs[0].architecture.is_empty());
        assert!(!mapped.source_jobs[0].required);

        // A coincidental job-id match on an auxiliary path must not inherit
        // reviewed typed identity.
        let mut coincidental = workflow.clone();
        coincidental.source_jobs = vec![LiveSourceJob {
            job_id: "unit".to_owned(),
            raw_object_refs: vec!["raw-src".to_owned()],
        }];
        let mapped = map_workflow(
            &coincidental,
            &repository,
            &manifest,
            &raw_by_id,
            &request_by_id,
            "2026-09-20T00:00:00Z",
        )
        .expect("auxiliary workflow keeps coincidental job IDs");
        assert_eq!(mapped.source_jobs[0].job_id, "unit");
        assert!(mapped.source_jobs[0].workload_id.is_empty());
        assert!(!mapped.source_jobs[0].required);

        let mut reviewed_mismatch = workflow.clone();
        reviewed_mismatch.path = ".github/workflows/ci.yml".to_owned();
        let err = map_workflow(
            &reviewed_mismatch,
            &repository,
            &manifest,
            &raw_by_id,
            &request_by_id,
            "2026-09-20T00:00:00Z",
        )
        .expect_err("reviewed-path mismatch still bails");
        assert_eq!(
            err.to_string(),
            "workflow source job release is absent from reviewed manifest"
        );

        let mut reviewed_match = reviewed_mismatch;
        reviewed_match.source_jobs = vec![LiveSourceJob {
            job_id: "unit".to_owned(),
            raw_object_refs: vec!["raw-src".to_owned()],
        }];
        let mapped = map_workflow(
            &reviewed_match,
            &repository,
            &manifest,
            &raw_by_id,
            &request_by_id,
            "2026-09-20T00:00:00Z",
        )
        .expect("reviewed workflow keeps typed identity");
        assert_eq!(mapped.source_jobs[0].workload_id, "tailrocks/example:unit");
        assert!(mapped.source_jobs[0].required);
    }

    #[test]
    fn bound_model_object_digest_tamper_fails_closed() {
        let model = raw_object(
            "model.session",
            "model-1",
            serde_json::json!({
                "session_id": "session-1",
                "effective": true,
                "orchestrator_model": "gpt-6-astra",
                "orchestrator_effort": "low",
                "agents": [],
                "raw_object_refs": ["model-1"]
            }),
        );
        let mut tampered = model;
        tampered.bytes_base64 = BASE64.encode(b"{}");
        assert!(CapturedModelSession::from_raw_object(&tampered).is_err());
    }
}
