//! Strict mapping from the live GitHub observation ledger to the checker G0
//! contract.  This adapter is deliberately fail-closed: optional provider
//! fields, unresolved action revisions, missing rate/request identities, and
//! external checks without workflow associations are errors, never synthetic
//! IDs or successful empty rows.

use super::live_collector::{
    LiveCheck, LiveCheckoutObservation, LiveCollection, LiveDependency, LiveExecution,
    LivePullRequest, LivePullRequestIdentity, LiveRepository, LiveWorkflow,
};
use super::{sha256_digest, AcquisitionState, ApiKind, HttpMethod, RawObjectRef, RequestRecord};
use crate::evidence_check::{ManifestDocument, ManifestRepository, CANONICAL_REPOSITORIES};
use crate::g0_contract::*;
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use url::Url;

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
    /// Raw objects captured by the producer-owned local binding boundary.
    /// These are merged with the provider ledger only after exact safe-byte
    /// hash/length/storage validation; a typed value or URI alone is never
    /// enough to enter the snapshot.
    pub local_raw_objects: Vec<RawObjectRef>,
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
        if !self
            .local_raw_objects
            .iter()
            .any(|raw| raw.raw_id == self.model_session.raw_object_ref)
            || !self
                .local_raw_objects
                .iter()
                .any(|raw| raw.raw_id == self.workload_artifact.raw_object_ref)
        {
            bail!("model and workload raw objects must be supplied to the mapping boundary");
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

fn validate_fixed_repository_set(manifest: &ManifestDocument, live: &LiveCollection) -> Result<()> {
    if manifest.repositories.len() != 32 || live.repositories.len() != 32 {
        bail!(
            "G0 mapping requires exactly 32 manifest and live repositories (manifest={}, live={})",
            manifest.repositories.len(),
            live.repositories.len()
        );
    }
    let canonical_names = CANONICAL_REPOSITORIES
        .iter()
        .map(|repository| (*repository).to_owned())
        .collect::<Vec<_>>();
    let manifest_names = manifest
        .repositories
        .iter()
        .map(|repository| repository.repository.clone())
        .collect::<Vec<_>>();
    let live_names = live
        .repositories
        .iter()
        .map(|repository| repository.repository.clone())
        .collect::<Vec<_>>();
    validate_fixed_repository_names(&canonical_names, &manifest_names)?;
    validate_fixed_repository_names(&manifest_names, &live_names)
}

fn validate_fixed_repository_names(manifest_names: &[String], live_names: &[String]) -> Result<()> {
    let manifest_set = manifest_names.iter().cloned().collect::<BTreeSet<_>>();
    if manifest_set.len() != manifest_names.len() {
        bail!("G0 manifest repository census contains duplicate names");
    }
    let live_set = live_names.iter().cloned().collect::<BTreeSet<_>>();
    if live_set.len() != live_names.len() {
        bail!("G0 live repository census contains duplicate names");
    }
    if manifest_set != live_set {
        let missing = manifest_set
            .difference(&live_set)
            .cloned()
            .collect::<Vec<_>>();
        let extra = live_set
            .difference(&manifest_set)
            .cloned()
            .collect::<Vec<_>>();
        bail!("G0 manifest/live repository census differs: missing={missing:?}, extra={extra:?}");
    }
    Ok(())
}

fn merge_raw_objects(
    live: &LiveCollection,
    bindings: &G0MappingBindings,
) -> Result<Vec<RawObjectRef>> {
    merge_raw_object_sets(
        &live.raw_objects,
        &bindings.local_raw_objects,
        &bindings.model_session.raw_object_ref,
        &bindings.workload_artifact.raw_object_ref,
    )
}

fn merge_raw_object_sets(
    provider_objects: &[RawObjectRef],
    local_objects: &[RawObjectRef],
    model_raw_id: &str,
    workload_raw_id: &str,
) -> Result<Vec<RawObjectRef>> {
    let mut merged = BTreeMap::<String, RawObjectRef>::new();
    let mut provider_ids = BTreeSet::new();
    for raw in provider_objects {
        if !provider_ids.insert(raw.raw_id.as_str()) {
            bail!("provider raw ledger repeats raw object {}", raw.raw_id);
        }
    }
    let mut local_ids = BTreeSet::new();
    for raw in local_objects {
        if !local_ids.insert(raw.raw_id.as_str()) {
            bail!("local binding ledger repeats raw object {}", raw.raw_id);
        }
    }
    for raw in provider_objects.iter().chain(local_objects) {
        // Validate the bytes and both canonical storage references before the
        // object enters the map.  The producer store has already verified the
        // immutable object; this second boundary prevents a caller from
        // replacing that verified reference with a typed-only claim.
        map_raw_object(raw)?;
        if let Some(existing) = merged.insert(raw.raw_id.clone(), raw.clone())
            && existing != *raw
        {
            bail!(
                "raw object ID {} has conflicting provider/local bindings",
                raw.raw_id
            );
        }
    }
    for (label, raw_id) in [
        ("model session", model_raw_id),
        ("workload artifact", workload_raw_id),
    ] {
        let raw = merged
            .get(raw_id)
            .ok_or_else(|| anyhow!("{label} raw object {raw_id} is absent after merge"))?;
        let expected_kind = if label == "model session" {
            "model.session"
        } else {
            "workload.artifact"
        };
        if raw.object_kind != expected_kind {
            bail!(
                "{label} raw object {raw_id} has unexpected object kind {}",
                raw.object_kind
            );
        }
    }
    Ok(merged.into_values().collect())
}

fn index_unique_requests(requests: &[RequestRecord]) -> Result<BTreeMap<String, &RequestRecord>> {
    let mut indexed = BTreeMap::new();
    for request in requests {
        if indexed
            .insert(request.request_id.clone(), request)
            .is_some()
        {
            bail!("live request ledger repeats request {}", request.request_id);
        }
    }
    Ok(indexed)
}

pub fn map_g0_inventory_with_supplement(
    live: &LiveCollection,
    manifest: &ManifestDocument,
    bindings: &G0MappingBindings,
) -> Result<G0MappedInventory> {
    bindings.validate()?;
    validate_fixed_repository_set(manifest, live)?;
    let merged_raw_objects = merge_raw_objects(live, bindings)?;
    let raw_by_id = merged_raw_objects
        .iter()
        .map(|raw| (raw.raw_id.clone(), raw))
        .collect::<BTreeMap<_, _>>();
    let request_by_id = index_unique_requests(&live.requests)?;
    let requests = live
        .requests
        .iter()
        .map(map_request)
        .collect::<Result<Vec<_>>>()?;
    let raw_objects = merged_raw_objects
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
        mode: "read_only".to_owned(),
        api_base: "https://api.github.com".to_owned(),
        api_versions: vec![GITHUB_API_VERSION.to_owned()],
    };
    let snapshot = G0CollectorSnapshot {
        schema_version: 2,
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
    let supplement = map_supplement(live, &merged_raw_objects, &canonical);
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

fn map_supplement(
    live: &LiveCollection,
    merged_raw_objects: &[RawObjectRef],
    canonical: &[u8],
) -> G0MappingSupplement {
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
    let raw_objects = merged_raw_objects
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
            validate_github_api_url(
                &ruleset.source_url,
                &format!(
                    "/repos/{}/rulesets/{}",
                    repository.repository, ruleset.ruleset_id
                ),
                &format!("ruleset {} source URL", ruleset.ruleset_id),
            )?;
            validate_nested_raw_references(
                &ruleset.raw_object_refs,
                raw_by_id,
                request_by_id,
                &format!("ruleset {}", ruleset.ruleset_id),
                &[
                    RawEndpointBinding {
                        object_kind: "rulesets",
                        endpoint: format!("/repos/{}/rulesets", repository.repository),
                    },
                    RawEndpointBinding {
                        object_kind: "ruleset",
                        endpoint: format!(
                            "/repos/{}/rulesets/{}",
                            repository.repository, ruleset.ruleset_id
                        ),
                    },
                ],
            )?;
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
                        if check.ruleset_id != ruleset.ruleset_id {
                            bail!(
                                "ruleset {} required check {} has foreign ruleset identity",
                                ruleset.ruleset_id,
                                check.context
                            );
                        }
                        if check.raw_object_refs != ruleset.raw_object_refs {
                            bail!(
                                "ruleset {} required check {} has unbound raw identity",
                                ruleset.ruleset_id,
                                check.context
                            );
                        }
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
        .map(|pr| map_pull_request(pr, manifest, repository, raw_by_id, request_by_id))
        .collect::<Result<Vec<_>>>()?;
    let main_checks = repository
        .main_checks
        .iter()
        .map(|check| {
            map_check(
                check,
                &repository.workflows,
                Some(&repository.main_executions),
                &repository.repository,
                raw_by_id,
                request_by_id,
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
    let source_jobs = workflow
        .source_jobs
        .iter()
        .map(|job| {
            let expected = manifest
                .expected_jobs
                .iter()
                .find(|expected| expected.job_id == job.job_id)
                .ok_or_else(|| {
                    anyhow!(
                        "workflow source job {} is absent from reviewed manifest",
                        job.job_id
                    )
                })?;
            validate_raw_references(
                &job.raw_object_refs,
                raw_by_id,
                request_by_id,
                &format!("workflow source job {}", job.job_id),
                &["workflow.source"],
            )?;
            Ok(G0SourceJob {
                job_id: job.job_id.clone(),
                workload_id: expected.workload_id.clone(),
                provider: expected.provider.clone(),
                platform: expected.platform.clone(),
                architecture: expected.architecture.clone(),
                required: expected.required,
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

struct RawEndpointBinding {
    object_kind: &'static str,
    endpoint: String,
}

fn validate_nested_raw_references(
    raw_ids: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    label: &str,
    expected: &[RawEndpointBinding],
) -> Result<()> {
    if raw_ids.is_empty() {
        bail!("{label} lacks raw object references");
    }
    let allowed_kinds = expected
        .iter()
        .map(|binding| binding.object_kind)
        .collect::<Vec<_>>();
    validate_raw_references(raw_ids, raw_by_id, request_by_id, label, &allowed_kinds)?;
    let expected_by_kind = expected
        .iter()
        .map(|binding| (binding.object_kind, binding))
        .collect::<BTreeMap<_, _>>();
    let mut observed_kinds = BTreeSet::new();
    let mut seen_raw_ids = BTreeSet::new();
    for raw_id in raw_ids {
        if !seen_raw_ids.insert(raw_id) {
            bail!("{label} repeats raw object {raw_id}");
        }
        let raw = raw_by_id
            .get(raw_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} references missing raw object {raw_id}"))?;
        let binding = expected_by_kind
            .get(raw.object_kind.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "{label} raw object {raw_id} has unexpected object kind {}",
                    raw.object_kind
                )
            })?;
        let request = request_by_id
            .get(&raw.request_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} raw object {raw_id} has missing request"))?;
        if request.api != ApiKind::Rest
            || request.method != HttpMethod::Get
            || request.endpoint_or_operation != binding.endpoint
        {
            bail!(
                "{label} raw object {raw_id} is bound to {} instead of {} {}",
                request.endpoint_or_operation,
                binding.object_kind,
                binding.endpoint
            );
        }
        observed_kinds.insert(raw.object_kind.as_str());
    }
    for binding in expected {
        if !observed_kinds.contains(binding.object_kind) {
            bail!(
                "{label} lacks raw object kind {} at {}",
                binding.object_kind,
                binding.endpoint
            );
        }
    }
    Ok(())
}

fn validate_github_html_url(value: &str, expected_path: &str, label: &str) -> Result<()> {
    let parsed = Url::parse(value).with_context(|| format!("parse {label}"))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != expected_path
    {
        bail!("{label} is not bound to https://github.com{expected_path}");
    }
    Ok(())
}

fn validate_github_api_url(value: &str, expected_path: &str, label: &str) -> Result<()> {
    let parsed = Url::parse(value).with_context(|| format!("parse {label}"))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("api.github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != expected_path
    {
        bail!("{label} is not bound to https://api.github.com{expected_path}");
    }
    Ok(())
}

fn require_all_attempts_query(
    raw_ids: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    label: &str,
) -> Result<()> {
    for raw_id in raw_ids {
        let raw = raw_by_id
            .get(raw_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} references missing raw object {raw_id}"))?;
        if raw.object_kind != "check_suite_runs" {
            continue;
        }
        let request = request_by_id
            .get(&raw.request_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} raw object {raw_id} has missing request"))?;
        let query = BASE64
            .decode(&request.query_base64)
            .with_context(|| format!("decode {label} request query"))?;
        let fields = std::str::from_utf8(&query)
            .with_context(|| format!("{label} request query is not UTF-8"))?
            .split(' ')
            .collect::<Vec<_>>();
        if fields.last() != Some(&"") || fields.len() % 2 == 0 {
            bail!("{label} request query has malformed canonical fields");
        }
        let (pair_fields, remainder) = fields.as_chunks::<2>();
        if remainder != [""] {
            bail!("{label} request query has malformed canonical fields");
        }
        let pairs = pair_fields
            .iter()
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>();
        if !pairs
            .iter()
            .any(|(key, value)| *key == "filter" && *value == "all")
            || pairs
                .iter()
                .any(|(key, value)| *key == "filter" && *value != "all")
        {
            bail!("{label} raw object {raw_id} was not acquired with filter=all");
        }
    }
    Ok(())
}

fn map_pull_request(
    pull_request: &LivePullRequest,
    manifest: &ManifestRepository,
    repository: &LiveRepository,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
) -> Result<G0PullRequestInventory> {
    let identity = &pull_request.identity;
    validate_github_html_url(
        &identity.source_url,
        &format!("{}/pull/{}", repository.repository, identity.number),
        &format!("PR #{} source URL", identity.number),
    )?;
    if pull_request.raw_object_refs != identity.raw_object_refs {
        bail!(
            "PR #{} outer raw identity differs from its detail identity",
            identity.number
        );
    }
    validate_nested_raw_references(
        &identity.raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("PR #{} identity", identity.number),
        &[
            RawEndpointBinding {
                object_kind: "pull_requests",
                endpoint: format!("/repos/{}/pulls", repository.repository),
            },
            RawEndpointBinding {
                object_kind: "pull_request",
                endpoint: format!("/repos/{}/pulls/{}", repository.repository, identity.number),
            },
        ],
    )?;
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
        .map(|check| {
            map_check(
                check,
                &repository.workflows,
                Some(&pull_request.executions),
                &repository.repository,
                raw_by_id,
                request_by_id,
            )
        })
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
    repository: &str,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
) -> Result<G0CheckProducer> {
    let app_id = check
        .app_id
        .clone()
        .ok_or_else(|| anyhow!("check {} lacks provider App identity", check.context))?;
    let workflow_run_id = check.workflow_run_id.ok_or_else(|| {
        anyhow!(
            "external check {} lacks workflow association",
            check.context
        )
    })?;
    let check_run_attempt = check
        .run_attempt
        .ok_or_else(|| anyhow!("check {} lacks run attempt", check.context))?;
    validate_github_html_url(
        &check.source_url,
        &format!("{repository}/runs/{}", check.check_run_id),
        &format!("check {} source URL", check.context),
    )?;
    let check_suite_id = check
        .check_suite_id
        .ok_or_else(|| anyhow!("check {} lacks suite identity", check.context))?;
    validate_nested_raw_references(
        &check.raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("check {}", check.context),
        &[
            RawEndpointBinding {
                object_kind: "check_suites",
                endpoint: format!(
                    "/repos/{repository}/commits/{}/check-suites",
                    check.source_sha
                ),
            },
            RawEndpointBinding {
                object_kind: "check_suite_runs",
                endpoint: format!("/repos/{repository}/check-suites/{check_suite_id}/check-runs"),
            },
        ],
    )?;
    require_all_attempts_query(
        &check.raw_object_refs,
        raw_by_id,
        request_by_id,
        &format!("check {}", check.context),
    )?;
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
        check_suite_id,
        check_run_id: check.check_run_id,
        workflow_run_id,
        run_attempt,
        job_id,
        source_sha: check.source_sha.clone(),
        actual_checkout_sha,
        event,
        status: check.status.clone(),
        conclusion: check.conclusion.clone().unwrap_or_else(|| "".to_owned()),
        source_url: check.source_url.clone(),
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
        validate_workflow_source_raw_references(
            &manifest_repository.repository,
            &workflow.path,
            &workflow.source_sha,
            &workflow.source_raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!("dependency graph source {}", manifest_repository.repository),
            "workflow.source",
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
            append_graph_node(
                &mut nodes,
                &mut graph_raw_ids,
                G0GraphNode {
                    id: workload_node_id.clone(),
                    kind: "workload".to_owned(),
                    repository: manifest_repository.repository.clone(),
                    workload_id: workload.clone(),
                    applicability: "required".to_owned(),
                    source_sha: workflow.source_sha.clone(),
                    source_ref: workflow.path.clone(),
                    raw_object_refs: source_raw_refs.clone(),
                },
            )?;
            append_required_check_edges(
                repository,
                manifest_repository,
                workflow,
                &workload,
                &workload_node_id,
                &source_raw_refs,
                raw_by_id,
                request_by_id,
                &mut nodes,
                &mut edges,
                &mut graph_raw_ids,
            )?;
            append_release_package_edges(
                manifest_repository,
                workflow,
                &workload,
                &workload_node_id,
                bindings,
                raw_by_id,
                &mut nodes,
                &mut edges,
                &mut graph_raw_ids,
            )?;
            for dependency in workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
            {
                if !matches!(
                    dependency.kind.as_str(),
                    "reusable_workflow" | "action" | "scanner"
                ) {
                    bail!(
                        "dependency {}/{} has unsupported typed kind {}",
                        dependency.repository,
                        dependency.path,
                        dependency.kind
                    );
                }
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
                let dependency_path = dependency
                    .resolved_path
                    .as_deref()
                    .unwrap_or(&dependency.path);
                validate_workflow_source_raw_references(
                    &dependency.repository,
                    dependency_path,
                    &dependency.revision,
                    &dependency.source_raw_object_refs,
                    raw_by_id,
                    request_by_id,
                    &format!(
                        "dependency graph source {}/{}@{}",
                        dependency.repository, dependency.path, dependency.revision
                    ),
                    "workflow.dependency.source",
                )?;
                if !is_sha(&dependency.revision) {
                    bail!(
                        "dependency {}/{} has unresolved revision {}",
                        dependency.repository,
                        dependency.path,
                        dependency.revision
                    );
                }
                let expected_child = manifest_repository
                    .expected_jobs
                    .iter()
                    .filter(|job| job.workload_id == workload)
                    .filter_map(|job| job.child_workflow.as_ref())
                    .find(|child| {
                        dependency.kind == "reusable_workflow"
                            && dependency.repository == child.repository
                            && dependency_path == child.workflow_path
                    });
                let (node_kind, edge_kind) = if let Some(child) = expected_child {
                    if !dependency_source_declares_event(dependency, &child.event)? {
                        bail!(
                            "child workflow {}/{} does not declare {}",
                            dependency.repository,
                            dependency_path,
                            child.event
                        );
                    }
                    ("child", "workload-to-child")
                } else {
                    ("workflow", "dependency")
                };
                let dependency_node_id = format!(
                    "{}:{}:{}:{}:{}@{}",
                    node_kind,
                    graph_component(&manifest_repository.repository),
                    graph_component(&workload),
                    graph_component(&dependency.repository),
                    graph_component(dependency_path),
                    dependency.revision
                );
                let dependency_raw_refs = merge_graph_refs(&[
                    &dependency.raw_object_refs,
                    &dependency.source_raw_object_refs,
                ])?;
                append_graph_node(
                    &mut nodes,
                    &mut graph_raw_ids,
                    G0GraphNode {
                        id: dependency_node_id.clone(),
                        kind: node_kind.to_owned(),
                        repository: dependency.repository.clone(),
                        workload_id: workload.clone(),
                        applicability: "required".to_owned(),
                        source_sha: dependency.revision.clone(),
                        source_ref: dependency_path.to_owned(),
                        raw_object_refs: dependency_raw_refs.clone(),
                    },
                )?;
                append_graph_edge(
                    &mut edges,
                    &mut graph_raw_ids,
                    G0GraphEdge {
                        from: workload_node_id.clone(),
                        to: dependency_node_id,
                        kind: edge_kind.to_owned(),
                        required: true,
                        source_sha: workflow.source_sha.clone(),
                        source_ref: workflow.path.clone(),
                        target_source_sha: dependency.revision.clone(),
                        target_source_ref: dependency_path.to_owned(),
                        raw_object_refs: merge_graph_refs(&[
                            &source_raw_refs,
                            &dependency_raw_refs,
                        ])?,
                    },
                )?;
            }
            let expected_children = manifest_repository
                .expected_jobs
                .iter()
                .filter(|job| job.workload_id == workload)
                .filter_map(|job| job.child_workflow.as_ref())
                .collect::<Vec<_>>();
            for child in expected_children {
                let found = workflow.reusable_workflows.iter().any(|dependency| {
                    dependency.kind == "reusable_workflow"
                        && dependency.repository == child.repository
                        && dependency
                            .resolved_path
                            .as_deref()
                            .unwrap_or(&dependency.path)
                            == child.workflow_path
                });
                if !found {
                    bail!(
                        "workload {} lacks source-bound child workflow {}/{}",
                        workload,
                        child.repository,
                        child.workflow_path
                    );
                }
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

fn append_graph_node(
    nodes: &mut Vec<G0GraphNode>,
    graph_raw_ids: &mut BTreeSet<String>,
    node: G0GraphNode,
) -> Result<()> {
    if node.id.trim().is_empty()
        || node.repository.trim().is_empty()
        || node.workload_id.trim().is_empty()
        || node.source_ref.trim().is_empty()
        || !is_sha(&node.source_sha)
        || !matches!(
            node.kind.as_str(),
            "artifact"
                | "check"
                | "child"
                | "package"
                | "release"
                | "source"
                | "workflow"
                | "workload"
        )
    {
        bail!("graph node {} has an invalid typed identity", node.id);
    }
    if node.raw_object_refs.is_empty() {
        bail!("graph node {} lacks raw source references", node.id);
    }
    if nodes.iter().any(|existing| existing.id == node.id) {
        bail!("graph node {} is duplicated", node.id);
    }
    graph_raw_ids.extend(node.raw_object_refs.iter().cloned());
    nodes.push(node);
    Ok(())
}

fn append_graph_edge(
    edges: &mut Vec<G0GraphEdge>,
    graph_raw_ids: &mut BTreeSet<String>,
    edge: G0GraphEdge,
) -> Result<()> {
    if edge.from.trim().is_empty()
        || edge.to.trim().is_empty()
        || edge.kind.trim().is_empty()
        || !is_sha(&edge.source_sha)
        || edge.source_ref.trim().is_empty()
        || !is_sha(&edge.target_source_sha)
        || edge.target_source_ref.trim().is_empty()
        || !matches!(
            edge.kind.as_str(),
            "consumes"
                | "dependency"
                | "produces"
                | "requires"
                | "workload-to-check"
                | "workload-to-child"
                | "workload-to-package"
                | "workload-to-release"
        )
    {
        bail!(
            "graph edge {} -> {} has an invalid typed identity",
            edge.from,
            edge.to
        );
    }
    if edge.raw_object_refs.is_empty() {
        bail!(
            "graph edge {} -> {} lacks raw source references",
            edge.from,
            edge.to
        );
    }
    let duplicate = edges.iter().any(|existing| {
        existing.from == edge.from
            && existing.to == edge.to
            && existing.kind == edge.kind
            && existing.required == edge.required
    });
    if duplicate {
        bail!("graph edge {} -> {} is duplicated", edge.from, edge.to);
    }
    graph_raw_ids.extend(edge.raw_object_refs.iter().cloned());
    edges.push(edge);
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "graph mapping keeps source, policy, request, and output bindings explicit"
)]
fn append_required_check_edges(
    repository: &LiveRepository,
    manifest: &ManifestRepository,
    workflow: &LiveWorkflow,
    workload: &str,
    workload_node_id: &str,
    source_raw_refs: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    nodes: &mut Vec<G0GraphNode>,
    edges: &mut Vec<G0GraphEdge>,
    graph_raw_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let expected = manifest
        .required_check_contexts_and_apps
        .iter()
        .map(|required| (required.context.clone(), required.app_id.clone()))
        .collect::<BTreeSet<_>>();
    if expected.is_empty() {
        bail!(
            "{} workload {} has no source-derived required-check contract",
            repository.repository,
            workload
        );
    }
    let mut observed = BTreeSet::new();
    let mut policies = Vec::new();
    for ruleset in &repository.rulesets {
        if !ruleset.complete {
            bail!(
                "ruleset {} in {} is incomplete",
                ruleset.ruleset_id,
                repository.repository
            );
        }
        validate_nested_raw_references(
            &ruleset.raw_object_refs,
            raw_by_id,
            request_by_id,
            &format!("graph ruleset {}", ruleset.ruleset_id),
            &[
                RawEndpointBinding {
                    object_kind: "rulesets",
                    endpoint: format!("/repos/{}/rulesets", repository.repository),
                },
                RawEndpointBinding {
                    object_kind: "ruleset",
                    endpoint: format!(
                        "/repos/{}/rulesets/{}",
                        repository.repository, ruleset.ruleset_id
                    ),
                },
            ],
        )?;
        for check in &ruleset.required_checks {
            let app_id = check.app_id.clone().ok_or_else(|| {
                anyhow!(
                    "ruleset {} check {} lacks provider App identity",
                    ruleset.ruleset_id,
                    check.context
                )
            })?;
            if check.ruleset_id != ruleset.ruleset_id
                || check.raw_object_refs != ruleset.raw_object_refs
            {
                bail!(
                    "ruleset {} check {} has an unbound policy identity",
                    ruleset.ruleset_id,
                    check.context
                );
            }
            let key = (check.context.clone(), app_id.clone());
            if !observed.insert((ruleset.ruleset_id, key.0.clone(), key.1.clone())) {
                bail!(
                    "ruleset {} repeats required check {}",
                    ruleset.ruleset_id,
                    check.context
                );
            }
            policies.push((ruleset.ruleset_id, key, check.raw_object_refs.clone()));
        }
    }
    if expected
        .iter()
        .any(|required| !policies.iter().any(|(_, key, _)| key == required))
    {
        let missing = expected
            .iter()
            .filter(|required| !policies.iter().any(|(_, key, _)| key == *required))
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "{} workload {} lacks live ruleset identities for required checks {missing:?}",
            repository.repository,
            workload
        );
    }
    for (ruleset_id, (context, app_id), policy_raw_refs) in policies {
        let required = expected.contains(&(context.clone(), app_id.clone()));
        let check_node_id = format!(
            "check:{}:{}:{}:{}",
            graph_component(&repository.repository),
            graph_component(workload),
            ruleset_id,
            graph_component(&format!("{context}\0{app_id}"))
        );
        append_graph_node(
            nodes,
            graph_raw_ids,
            G0GraphNode {
                id: check_node_id.clone(),
                kind: "check".to_owned(),
                repository: repository.repository.clone(),
                workload_id: workload.to_owned(),
                applicability: if required {
                    "required".to_owned()
                } else {
                    "applicable".to_owned()
                },
                source_sha: workflow.source_sha.clone(),
                source_ref: workflow.path.clone(),
                raw_object_refs: policy_raw_refs.clone(),
            },
        )?;
        append_graph_edge(
            edges,
            graph_raw_ids,
            G0GraphEdge {
                from: workload_node_id.to_owned(),
                to: check_node_id,
                kind: "workload-to-check".to_owned(),
                required,
                source_sha: workflow.source_sha.clone(),
                source_ref: workflow.path.clone(),
                target_source_sha: workflow.source_sha.clone(),
                target_source_ref: workflow.path.clone(),
                raw_object_refs: merge_graph_refs(&[source_raw_refs, &policy_raw_refs])?,
            },
        )?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "release/package graph mapping keeps reviewed and raw bindings explicit"
)]
fn append_release_package_edges(
    manifest: &ManifestRepository,
    workflow: &LiveWorkflow,
    workload: &str,
    workload_node_id: &str,
    bindings: &G0MappingBindings,
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    nodes: &mut Vec<G0GraphNode>,
    edges: &mut Vec<G0GraphEdge>,
    graph_raw_ids: &mut BTreeSet<String>,
) -> Result<()> {
    if !matches!(
        manifest.release_applicability,
        crate::evidence_check::Applicability::Required
            | crate::evidence_check::Applicability::Applicable
    ) {
        return Ok(());
    }
    if manifest.runtime_product_id.trim().is_empty()
        || manifest.runtime_release_version.trim().is_empty()
        || manifest.workflow_path.trim().is_empty()
    {
        bail!(
            "{} workload {} has release/package applicability without immutable identity",
            manifest.repository,
            workload
        );
    }
    let source_sha = [
        manifest.runtime_source_sha.as_str(),
        manifest.workflow_revision.as_str(),
        manifest.generator_revision.as_str(),
    ]
    .into_iter()
    .find(|candidate| is_sha(candidate))
    .map(str::to_owned)
    .ok_or_else(|| {
        anyhow!(
            "{} workload {} has no immutable release/package source revision",
            manifest.repository,
            workload
        )
    })?;
    let artifact_raw_refs = bindings.workload_artifact.value().raw_object_refs.clone();
    validate_local_graph_raw_references(
        &artifact_raw_refs,
        raw_by_id,
        &format!("{} workload artifact", manifest.repository),
    )?;
    let source_ref = manifest.workflow_path.clone();
    let release_node_id = format!(
        "release:{}:{}:{}:{}",
        graph_component(&manifest.repository),
        graph_component(workload),
        graph_component(&manifest.runtime_product_id),
        graph_component(&manifest.runtime_release_version)
    );
    let package_node_id = format!(
        "package:{}:{}:{}:{}",
        graph_component(&manifest.repository),
        graph_component(workload),
        graph_component(&manifest.runtime_product_id),
        graph_component(&manifest.runtime_release_version)
    );
    append_graph_node(
        nodes,
        graph_raw_ids,
        G0GraphNode {
            id: release_node_id.clone(),
            kind: "release".to_owned(),
            repository: manifest.repository.clone(),
            workload_id: workload.to_owned(),
            applicability: "required".to_owned(),
            source_sha: source_sha.clone(),
            source_ref: source_ref.clone(),
            raw_object_refs: artifact_raw_refs.clone(),
        },
    )?;
    append_graph_node(
        nodes,
        graph_raw_ids,
        G0GraphNode {
            id: package_node_id.clone(),
            kind: "package".to_owned(),
            repository: manifest.repository.clone(),
            workload_id: workload.to_owned(),
            applicability: "required".to_owned(),
            source_sha: source_sha.clone(),
            source_ref: source_ref.clone(),
            raw_object_refs: artifact_raw_refs.clone(),
        },
    )?;
    let edge_raw_refs = merge_graph_refs(&[&workflow.source_raw_object_refs, &artifact_raw_refs])?;
    append_graph_edge(
        edges,
        graph_raw_ids,
        G0GraphEdge {
            from: workload_node_id.to_owned(),
            to: release_node_id.clone(),
            kind: "workload-to-release".to_owned(),
            required: true,
            source_sha: workflow.source_sha.clone(),
            source_ref: workflow.path.clone(),
            target_source_sha: source_sha.clone(),
            target_source_ref: source_ref.clone(),
            raw_object_refs: edge_raw_refs.clone(),
        },
    )?;
    append_graph_edge(
        edges,
        graph_raw_ids,
        G0GraphEdge {
            from: workload_node_id.to_owned(),
            to: package_node_id.clone(),
            kind: "workload-to-package".to_owned(),
            required: true,
            source_sha: workflow.source_sha.clone(),
            source_ref: workflow.path.clone(),
            target_source_sha: source_sha.clone(),
            target_source_ref: source_ref.clone(),
            raw_object_refs: edge_raw_refs.clone(),
        },
    )?;
    // G0 has no public install node. Keep the package's immutable release
    // prerequisite typed; functional installation remains G2 InstallEvidence.
    append_graph_edge(
        edges,
        graph_raw_ids,
        G0GraphEdge {
            from: package_node_id,
            to: release_node_id,
            kind: "requires".to_owned(),
            required: true,
            source_sha: source_sha.clone(),
            source_ref: source_ref.clone(),
            target_source_sha: [
                manifest.runtime_source_sha.as_str(),
                manifest.workflow_revision.as_str(),
                manifest.generator_revision.as_str(),
            ]
            .into_iter()
            .find(|candidate| is_sha(candidate))
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!(
                    "{} workload {} has no immutable release/package source revision",
                    manifest.repository,
                    workload
                )
            })?,
            target_source_ref: manifest.workflow_path.clone(),
            raw_object_refs: edge_raw_refs,
        },
    )?;
    Ok(())
}

fn validate_local_graph_raw_references(
    raw_ids: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    label: &str,
) -> Result<()> {
    if raw_ids.is_empty() {
        bail!("{label} lacks raw source references");
    }
    let mut seen = BTreeSet::new();
    for raw_id in raw_ids {
        if !seen.insert(raw_id) {
            bail!("{label} repeats raw object {raw_id}");
        }
        let raw = raw_by_id
            .get(raw_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} references missing raw object {raw_id}"))?;
        if !matches!(
            raw.object_kind.as_str(),
            "workload.source" | "workload.artifact"
        ) {
            bail!(
                "{label} raw object {raw_id} has unexpected kind {}",
                raw.object_kind
            );
        }
    }
    Ok(())
}

fn merge_graph_refs(parts: &[&[String]]) -> Result<Vec<String>> {
    let mut refs = BTreeSet::new();
    for part in parts {
        refs.extend(part.iter().cloned());
    }
    if refs.is_empty() {
        bail!("graph relation lacks raw source references");
    }
    Ok(refs.into_iter().collect())
}

#[allow(
    clippy::too_many_arguments,
    reason = "source endpoint validation needs every identity and ledger boundary"
)]
fn validate_workflow_source_raw_references(
    repository: &str,
    path: &str,
    revision: &str,
    raw_ids: &[String],
    raw_by_id: &BTreeMap<String, &RawObjectRef>,
    request_by_id: &BTreeMap<String, &RequestRecord>,
    label: &str,
    object_kind: &str,
) -> Result<()> {
    validate_raw_references(raw_ids, raw_by_id, request_by_id, label, &[object_kind])?;
    let expected = format!(
        "/repos/{repository}/contents/{}?ref={revision}",
        encode_graph_api_path(path)
    );
    for raw_id in raw_ids {
        let raw = raw_by_id
            .get(raw_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} references missing raw object {raw_id}"))?;
        let request = request_by_id
            .get(&raw.request_id)
            .copied()
            .ok_or_else(|| anyhow!("{label} raw object {raw_id} has missing request"))?;
        if request.api != ApiKind::Rest
            || request.method != HttpMethod::Get
            || request.endpoint_or_operation != expected
        {
            bail!(
                "{label} raw object {raw_id} is bound to {} instead of {}",
                request.endpoint_or_operation,
                expected
            );
        }
    }
    Ok(())
}

fn encode_graph_api_path(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn dependency_source_declares_event(dependency: &LiveDependency, event: &str) -> Result<bool> {
    let bytes = BASE64
        .decode(dependency.source_bytes_base64.as_deref().ok_or_else(|| {
            anyhow!(
                "child workflow {}/{} lacks immutable source bytes",
                dependency.repository,
                dependency.path
            )
        })?)
        .context("decode child workflow source bytes")?;
    let value: YamlValue = serde_yaml::from_slice(&bytes).context("parse child workflow source")?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("child workflow source must be a YAML mapping"))?;
    let on = mapping
        .iter()
        .find_map(|(key, value)| (key == "on").then_some(value))
        .ok_or_else(|| anyhow!("child workflow source lacks an on declaration"))?;
    Ok(match on {
        YamlValue::String(value) => value == event,
        YamlValue::Sequence(values) => values.iter().any(|value| value.as_str() == Some(event)),
        YamlValue::Mapping(values) => values.keys().any(|key| key == event),
        _ => false,
    })
}

fn graph_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02x}"));
        }
    }
    encoded
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
    use crate::github_acquisition::live_collector::{LiveRuleset, LiveRulesetCheck, LiveSourceJob};
    use crate::github_acquisition::sha256_digest;
    use base64::engine::general_purpose::STANDARD as BASE64;

    fn raw_object(kind: &str, raw_id: &str, value: serde_json::Value) -> RawObjectRef {
        let bytes = serde_json::to_vec(&value).expect("fixture JSON");
        raw_bytes(kind, raw_id, &bytes)
    }

    fn raw_bytes(kind: &str, raw_id: &str, bytes: &[u8]) -> RawObjectRef {
        let digest = sha256_digest(bytes);
        let digest_hex = digest.strip_prefix("sha256:").expect("digest prefix");
        RawObjectRef {
            raw_id: raw_id.to_owned(),
            request_id: format!("{raw_id}-request"),
            object_kind: kind.to_owned(),
            canonicalization: "json-utf8".to_owned(),
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            bytes_base64: BASE64.encode(bytes),
            media_type: "application/json".to_owned(),
            storage_ref: format!("sha256://{digest_hex}"),
            original_sha256: digest.clone(),
            original_byte_length: bytes.len() as u64,
            original_storage_ref: format!("sha256://{digest_hex}"),
        }
    }

    fn rest_request(raw: &RawObjectRef, endpoint: &str) -> RequestRecord {
        RequestRecord {
            request_id: raw.request_id.clone(),
            api: ApiKind::Rest,
            method: HttpMethod::Get,
            endpoint_or_operation: endpoint.to_owned(),
            query_base64: BASE64.encode(b"{}"),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: Some(sha256_digest(b"{}")),
            variables_sha256: Some(sha256_digest(b"{}")),
            redacted_variables: None,
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            http_status: Some(200),
            api_request_id: Some("request".to_owned()),
            rate_limit: Some(crate::github_acquisition::RateLimitObservation {
                limit: Some(5000),
                remaining: Some(4999),
                used: Some(1),
                reset_at: Some("2026-09-20T01:00:00Z".to_owned()),
                retry_after: None,
            }),
            safe_scopes: Some(vec!["metadata:read".to_owned()]),
            page: crate::github_acquisition::PageState {
                number: 1,
                per_page: Some(100),
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: Some(false),
                items_returned: 1,
            },
            response_raw_ref: Some(raw.raw_id.clone()),
            error_raw_ref: None,
            state: AcquisitionState::Complete,
            complete: true,
            truncation_reason: None,
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

    #[test]
    fn fixed_repository_census_requires_exact_unique_set() {
        let expected = (0..32)
            .map(|index| format!("tailrocks/repo-{index:02}"))
            .collect::<Vec<_>>();
        assert!(validate_fixed_repository_names(&expected, &expected).is_ok());

        let mut duplicate = expected.clone();
        duplicate[31] = duplicate[0].clone();
        let error = validate_fixed_repository_names(&expected, &duplicate)
            .expect_err("duplicate live repository must fail closed");
        assert!(error.to_string().contains("duplicate"));

        let mut mismatched = expected.clone();
        mismatched[31] = "tailrocks/replacement".to_owned();
        let error = validate_fixed_repository_names(&expected, &mismatched)
            .expect_err("wrong 32-name set must fail closed");
        assert!(error.to_string().contains("differs"));
    }

    fn scope_manifest(names: &[String]) -> ManifestDocument {
        ManifestDocument {
            schema_version: 2,
            manifest_id: "github-first-dual-lane-2026-09-19".to_owned(),
            source: crate::evidence_check::SourceIdentity {
                repository: "tailrocks/velnor".to_owned(),
                revision: "a".repeat(40),
                digest: "sha256:".to_owned() + &"b".repeat(64),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: names
                .iter()
                .map(|repository| ManifestRepository {
                    repository: repository.clone(),
                    ..ManifestRepository::default()
                })
                .collect(),
        }
    }

    fn scope_live(names: &[String]) -> LiveCollection {
        LiveCollection {
            schema_version: 1,
            manifest_id: "github-first-dual-lane-2026-09-19".to_owned(),
            snapshot_id: "scope-fixture".to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            auth: crate::github_acquisition::AuthIdentity::new(
                "fixture-auth",
                "github",
                Some("1".to_owned()),
                Some("fixture".to_owned()),
                BTreeSet::from(["metadata:read".to_owned()]),
            ),
            requests: Vec::new(),
            raw_objects: Vec::new(),
            repositories: names
                .iter()
                .enumerate()
                .map(|(index, repository)| LiveRepository {
                    repository: repository.clone(),
                    repository_id: index as u64 + 1,
                    default_branch: "main".to_owned(),
                    default_branch_sha: "a".repeat(40),
                    rulesets: Vec::new(),
                    workflows: Vec::new(),
                    open_prs: Vec::new(),
                    artifacts: Vec::new(),
                    main_executions: Vec::new(),
                    main_checks: Vec::new(),
                    closing_repository_id: index as u64 + 1,
                    closing_default_branch: "main".to_owned(),
                    closing_default_branch_sha: "a".repeat(40),
                    source_invalidated: false,
                    access_state: "unknown".to_owned(),
                    access_gaps: vec!["fixture".to_owned()],
                    raw_object_refs: Vec::new(),
                })
                .collect(),
            opening_prs: Vec::new(),
            closing_prs: Vec::new(),
            reconciliation: crate::github_acquisition::IdentityReconciliation {
                opening: Vec::new(),
                closing: Vec::new(),
                changes: Vec::new(),
                duplicate_keys: Vec::new(),
                stable: true,
            },
        }
    }

    fn graph_fixture(
        release_applicability: crate::evidence_check::Applicability,
        required_child: bool,
        include_child: bool,
        include_check: bool,
        include_artifact_refs: bool,
    ) -> (
        ManifestDocument,
        LiveCollection,
        G0MappingBindings,
        Vec<RawObjectRef>,
        Vec<RequestRecord>,
    ) {
        let root_sha = "a".repeat(40);
        let child_sha = "b".repeat(40);
        let repository_name = "acme/repo".to_owned();
        let root_path = ".github/workflows/ci.yml".to_owned();
        let child_repository = "acme/child".to_owned();
        let child_path = ".github/workflows/child.yml".to_owned();
        let root_source = b"on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n";
        let child_source = b"on:\n  workflow_call:\njobs:\n  child:\n    runs-on: ubuntu-24.04\n";
        let root_raw = raw_bytes("workflow.source", "root-source", root_source);
        let rules_list = raw_object("rulesets", "rules-list", serde_json::json!([{"id": 7}]));
        let rules_detail = raw_object(
            "ruleset",
            "rules-detail",
            serde_json::json!({"id": 7, "name": "required-ci"}),
        );
        let child_tree = raw_object(
            "workflow.dependency",
            "child-tree",
            serde_json::json!({"tree": [child_path.clone()]}),
        );
        let child_raw = raw_bytes("workflow.dependency.source", "child-source", child_source);
        let model_raw = raw_object(
            "model.session",
            "model-local",
            serde_json::json!({"session": "fixture"}),
        );
        let workload_source_raw = raw_object(
            "workload.source",
            "workload-source",
            serde_json::json!({"source": "fixture"}),
        );
        let workload_artifact_raw = raw_object(
            "workload.artifact",
            "workload-artifact",
            serde_json::json!({"artifact": "fixture"}),
        );
        let mut raw_objects = vec![
            root_raw.clone(),
            rules_list.clone(),
            rules_detail.clone(),
            child_tree.clone(),
            child_raw.clone(),
            model_raw.clone(),
            workload_source_raw.clone(),
            workload_artifact_raw.clone(),
        ];
        let requests = vec![
            rest_request(
                &root_raw,
                &format!("/repos/{repository_name}/contents/{root_path}?ref={root_sha}"),
            ),
            rest_request(&rules_list, &format!("/repos/{repository_name}/rulesets")),
            rest_request(
                &rules_detail,
                &format!("/repos/{repository_name}/rulesets/7"),
            ),
            rest_request(
                &child_tree,
                &format!("/repos/{child_repository}/git/trees/{child_sha}?recursive=1"),
            ),
            rest_request(
                &child_raw,
                &format!("/repos/{child_repository}/contents/{child_path}?ref={child_sha}"),
            ),
        ];
        let dependency = LiveDependency {
            kind: "reusable_workflow".to_owned(),
            repository: child_repository.clone(),
            path: child_path.clone(),
            revision: child_sha.clone(),
            resolved_path: Some(child_path.clone()),
            source_url: None,
            source_bytes_base64: Some(BASE64.encode(child_source)),
            source_raw_object_refs: vec![child_raw.raw_id.clone()],
            raw_object_refs: vec![child_tree.raw_id.clone()],
        };
        let workflow = LiveWorkflow {
            path: root_path.clone(),
            revision: root_sha.clone(),
            source_sha: root_sha.clone(),
            source_url: format!("https://github.com/{repository_name}/blob/{root_sha}/{root_path}"),
            source_bytes_base64: BASE64.encode(root_source),
            source_raw_object_refs: vec![root_raw.raw_id.clone()],
            events: vec!["push".to_owned()],
            source_jobs: vec![LiveSourceJob {
                job_id: "scan".to_owned(),
                raw_object_refs: vec![root_raw.raw_id.clone()],
            }],
            reusable_workflows: if include_child {
                vec![dependency]
            } else {
                Vec::new()
            },
            actions: Vec::new(),
            scanners: Vec::new(),
            raw_object_refs: vec![root_raw.raw_id.clone()],
        };
        let ruleset = LiveRuleset {
            ruleset_id: 7,
            name: "required-ci".to_owned(),
            source_url: format!("https://api.github.com/repos/{repository_name}/rulesets/7"),
            complete: true,
            required_checks: if include_check {
                vec![LiveRulesetCheck {
                    context: "ci".to_owned(),
                    app_id: Some("123".to_owned()),
                    ruleset_id: 7,
                    raw_object_refs: vec![rules_list.raw_id.clone(), rules_detail.raw_id.clone()],
                }]
            } else {
                Vec::new()
            },
            raw_object_refs: vec![rules_list.raw_id.clone(), rules_detail.raw_id.clone()],
        };
        let live_repository = LiveRepository {
            repository: repository_name.clone(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: root_sha.clone(),
            rulesets: vec![ruleset],
            workflows: vec![workflow],
            open_prs: Vec::new(),
            artifacts: Vec::new(),
            main_executions: Vec::new(),
            main_checks: Vec::new(),
            closing_repository_id: 1,
            closing_default_branch: "main".to_owned(),
            closing_default_branch_sha: root_sha.clone(),
            source_invalidated: false,
            access_state: "observed".to_owned(),
            access_gaps: Vec::new(),
            raw_object_refs: vec![root_raw.raw_id.clone()],
        };
        let manifest_repository = ManifestRepository {
            repository: repository_name.clone(),
            repository_role: "library".to_owned(),
            default_branch: "main".to_owned(),
            expected_workload_ids: vec!["scan".to_owned()],
            required_check_contexts_and_apps: vec![crate::evidence_check::RequiredContext {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
            }],
            workload_platform_architecture: vec![crate::evidence_check::WorkloadPlatform {
                workload_id: "scan".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
            }],
            expected_jobs: vec![crate::evidence_check::ExpectedJobSpec {
                job_id: "scan".to_owned(),
                workload_id: "scan".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
                required: true,
                child_workflow: required_child.then_some(
                    crate::evidence_check::ChildWorkflowSpec {
                        repository: child_repository.clone(),
                        workflow_path: child_path.clone(),
                        event: "workflow_call".to_owned(),
                    },
                ),
            }],
            generated_plan_digest: "sha256:".to_owned() + &"c".repeat(64),
            workflow_path: root_path,
            workflow_revision: root_sha.clone(),
            provider_eligibility: BTreeMap::new(),
            host_contracts: BTreeMap::new(),
            release_applicability,
            generator_revision: root_sha.clone(),
            runtime_product_id: "velnor".to_owned(),
            generator_artifact_digest: "sha256:".to_owned() + &"d".repeat(64),
            configuration_digest: "sha256:".to_owned() + &"e".repeat(64),
            generated_tree_digest: "sha256:".to_owned() + &"f".repeat(64),
            scan_state_digest: "sha256:".to_owned() + &"0".repeat(64),
            runtime_release_version: "1.0.0".to_owned(),
            runtime_source_sha: root_sha,
            job_image_digest: "sha256:".to_owned() + &"1".repeat(64),
        };
        let manifest = ManifestDocument {
            schema_version: 2,
            manifest_id: "manifest".to_owned(),
            source: crate::evidence_check::SourceIdentity {
                repository: repository_name,
                revision: "c".repeat(40),
                digest: "sha256:".to_owned() + &"2".repeat(64),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: vec![manifest_repository],
        };
        if !include_artifact_refs {
            raw_objects.retain(|raw| raw.raw_id != workload_source_raw.raw_id);
        }
        let artifact_refs = if include_artifact_refs {
            vec![
                workload_source_raw.raw_id,
                workload_artifact_raw.raw_id.clone(),
            ]
        } else {
            Vec::new()
        };
        let artifact_digest = sha256_digest(b"workload-artifact");
        let bindings = G0MappingBindings {
            collector_name: "fixture".to_owned(),
            collector_revision: "a".repeat(40),
            phase: "G0".to_owned(),
            model_session: CapturedModelSession {
                value: G0ModelSession {
                    session_id: "fixture".to_owned(),
                    effective: true,
                    orchestrator_model: "gpt-6-astra".to_owned(),
                    orchestrator_effort: "low".to_owned(),
                    agents: vec![G0AgentModel {
                        agent_id: "fixture".to_owned(),
                        model: "gpt-5.6-luna".to_owned(),
                        effort: "max".to_owned(),
                        effective: true,
                        raw_object_refs: vec![model_raw.raw_id.clone()],
                    }],
                    raw_object_refs: vec![model_raw.raw_id.clone()],
                },
                raw_object_ref: model_raw.raw_id,
            },
            workload_artifact: CapturedWorkloadArtifact {
                value: G0ArtifactReference {
                    name: "fixture".to_owned(),
                    schema: "fixture.v1".to_owned(),
                    source_url: "https://github.com/acme/repo/tree/a".to_owned(),
                    sha256: artifact_digest.clone(),
                    storage_ref: format!(
                        "sha256://{}",
                        artifact_digest.strip_prefix("sha256:").expect("digest")
                    ),
                    source_revision: "a".repeat(40),
                    source_digest: artifact_digest,
                    observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                    raw_object_refs: artifact_refs,
                },
                raw_object_ref: workload_artifact_raw.raw_id,
            },
            local_raw_objects: Vec::new(),
            collector_snapshot_storage_ref: "sha256://".to_owned() + &"3".repeat(64),
            workflow_workloads: BTreeMap::from([(
                (
                    "acme/repo".to_owned(),
                    ".github/workflows/ci.yml".to_owned(),
                ),
                vec!["scan".to_owned()],
            )]),
        };
        let live = LiveCollection {
            schema_version: 1,
            manifest_id: "manifest".to_owned(),
            snapshot_id: "snapshot".to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            auth: crate::github_acquisition::AuthIdentity::new(
                "fixture-auth",
                "github",
                Some("1".to_owned()),
                Some("fixture".to_owned()),
                BTreeSet::from(["actions:read".to_owned()]),
            ),
            requests: requests.clone(),
            raw_objects: raw_objects.clone(),
            repositories: vec![live_repository],
            opening_prs: Vec::new(),
            closing_prs: Vec::new(),
            reconciliation: crate::github_acquisition::IdentityReconciliation {
                opening: Vec::new(),
                closing: Vec::new(),
                changes: Vec::new(),
                duplicate_keys: Vec::new(),
                stable: true,
            },
        };
        (manifest, live, bindings, raw_objects, requests)
    }

    #[test]
    fn dependency_graph_maps_source_bound_check_child_release_package_edges() {
        let (manifest, live, bindings, raw_objects, requests) = graph_fixture(
            crate::evidence_check::Applicability::Required,
            true,
            true,
            true,
            true,
        );
        let raw_by_id = raw_objects
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        let request_by_id = requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        let graph = map_dependency_graph(&live, &manifest, &bindings, &raw_by_id, &request_by_id)
            .expect("complete source-bound graph fixture");
        let edge_kinds = graph
            .edges
            .iter()
            .map(|edge| edge.kind.as_str())
            .collect::<BTreeSet<_>>();
        assert!(edge_kinds.contains("workload-to-check"));
        assert!(edge_kinds.contains("workload-to-child"));
        assert!(edge_kinds.contains("workload-to-release"));
        assert!(edge_kinds.contains("workload-to-package"));
        assert!(edge_kinds.contains("requires"));
        assert!(graph.nodes.iter().any(|node| node.kind == "check"));
        assert!(graph.nodes.iter().any(|node| node.kind == "child"));
        assert!(graph.nodes.iter().any(|node| node.kind == "release"));
        assert!(graph.nodes.iter().any(|node| node.kind == "package"));
        assert!(graph
            .edges
            .iter()
            .all(|edge| !edge.raw_object_refs.is_empty()));
    }

    #[test]
    fn dependency_graph_rejects_omitted_required_check_edge() {
        let (manifest, live, bindings, raw_objects, requests) = graph_fixture(
            crate::evidence_check::Applicability::NotApplicable,
            false,
            false,
            false,
            true,
        );
        let raw_by_id = raw_objects
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        let request_by_id = requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        let error = map_dependency_graph(&live, &manifest, &bindings, &raw_by_id, &request_by_id)
            .expect_err("missing required policy must fail closed");
        assert!(error.to_string().contains("required checks"));
    }

    #[test]
    fn dependency_graph_rejects_omitted_child_edge() {
        let (manifest, live, bindings, raw_objects, requests) = graph_fixture(
            crate::evidence_check::Applicability::NotApplicable,
            true,
            false,
            true,
            true,
        );
        let raw_by_id = raw_objects
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        let request_by_id = requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        let error = map_dependency_graph(&live, &manifest, &bindings, &raw_by_id, &request_by_id)
            .expect_err("missing child source must fail closed");
        assert!(error.to_string().contains("child workflow"));
    }

    #[test]
    fn dependency_graph_rejects_omitted_release_package_source() {
        let (manifest, live, bindings, raw_objects, requests) = graph_fixture(
            crate::evidence_check::Applicability::Required,
            false,
            false,
            true,
            false,
        );
        let raw_by_id = raw_objects
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        let request_by_id = requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        let error = map_dependency_graph(&live, &manifest, &bindings, &raw_by_id, &request_by_id)
            .expect_err("missing release/package raw association must fail closed");
        assert!(error.to_string().contains("workload artifact"));
    }

    #[test]
    fn mapping_scope_preflight_accepts_canonical_production_census() {
        let names = CANONICAL_REPOSITORIES
            .iter()
            .map(|repository| (*repository).to_owned())
            .collect::<Vec<_>>();
        let manifest = scope_manifest(&names);
        let live = scope_live(&names);
        assert!(validate_fixed_repository_set(&manifest, &live).is_ok());
    }

    #[test]
    fn mapping_scope_preflight_rejects_substitution_before_capture() {
        let canonical = CANONICAL_REPOSITORIES
            .iter()
            .map(|repository| (*repository).to_owned())
            .collect::<Vec<_>>();
        let mut substituted = canonical.clone();
        substituted[0] = "tailrocks/not-in-scope".to_owned();
        let manifest = scope_manifest(&substituted);
        let live = scope_live(&substituted);
        let error = validate_fixed_repository_set(&manifest, &live)
            .expect_err("substituted scope must fail before nested mapping");
        assert!(error.to_string().contains("differs"));
    }

    #[test]
    fn local_binding_raw_objects_merge_only_with_exact_provenance() {
        let provider = raw_object("repository", "provider-1", serde_json::json!({"id": 1}));
        let model = raw_object("model.session", "model-1", serde_json::json!({"model": 1}));
        let workload = raw_object(
            "workload.artifact",
            "workload-1",
            serde_json::json!({"workload": 1}),
        );
        let merged = merge_raw_object_sets(
            &[provider],
            &[model.clone(), workload.clone()],
            "model-1",
            "workload-1",
        )
        .expect("provider and local objects with exact store refs merge");
        assert_eq!(
            merged
                .iter()
                .map(|raw| raw.raw_id.as_str())
                .collect::<Vec<_>>(),
            vec!["model-1", "provider-1", "workload-1"]
        );

        let mut tampered = model;
        tampered.byte_length += 1;
        assert!(
            merge_raw_object_sets(&[], &[tampered, workload.clone()], "model-1", "workload-1",)
                .is_err()
        );

        let mut conflicting = workload.clone();
        conflicting.object_kind = "caller.claim".to_owned();
        assert!(
            merge_raw_object_sets(&[], &[workload, conflicting], "model-1", "workload-1",).is_err()
        );
    }

    #[test]
    fn nested_raw_identity_binds_kind_to_exact_request_endpoint() {
        let list = raw_object("pull_requests", "pr-list", serde_json::json!([1]));
        let detail = raw_object(
            "pull_request",
            "pr-detail",
            serde_json::json!({"number": 7}),
        );
        let list_endpoint = "/repos/tailrocks/example/pulls";
        let detail_endpoint = "/repos/tailrocks/example/pulls/7";
        let raw_values = [list.clone(), detail.clone()];
        let raw_by_id = raw_values
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        let requests = [
            rest_request(&list, list_endpoint),
            rest_request(&detail, detail_endpoint),
        ];
        let request_by_id = requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        let raw_ids = vec![list.raw_id.clone(), detail.raw_id.clone()];
        let expected = [
            RawEndpointBinding {
                object_kind: "pull_requests",
                endpoint: list_endpoint.to_owned(),
            },
            RawEndpointBinding {
                object_kind: "pull_request",
                endpoint: detail_endpoint.to_owned(),
            },
        ];
        assert!(validate_nested_raw_references(
            &raw_ids,
            &raw_by_id,
            &request_by_id,
            "PR #7",
            &expected,
        )
        .is_ok());

        let mut wrong_endpoint = requests[1].clone();
        wrong_endpoint.endpoint_or_operation = "/repos/tailrocks/example/rulesets/7".to_owned();
        let wrong_requests = [requests[0].clone(), wrong_endpoint];
        let wrong_request_by_id = wrong_requests
            .iter()
            .map(|request| (request.request_id.clone(), request))
            .collect::<BTreeMap<_, _>>();
        assert!(validate_nested_raw_references(
            &raw_ids,
            &raw_by_id,
            &wrong_request_by_id,
            "PR #7",
            &expected,
        )
        .is_err());

        let mut wrong_kind_detail = detail.clone();
        wrong_kind_detail.object_kind = "ruleset".to_owned();
        let wrong_kind_values = [list.clone(), wrong_kind_detail];
        let wrong_kind_by_id = wrong_kind_values
            .iter()
            .map(|raw| (raw.raw_id.clone(), raw))
            .collect::<BTreeMap<_, _>>();
        assert!(validate_nested_raw_references(
            &raw_ids,
            &wrong_kind_by_id,
            &request_by_id,
            "PR #7",
            &expected,
        )
        .is_err());

        let duplicate = vec![list.raw_id.clone(), list.raw_id];
        assert!(validate_nested_raw_references(
            &duplicate,
            &raw_by_id,
            &request_by_id,
            "PR #7",
            &expected,
        )
        .is_err());
    }

    #[test]
    fn check_run_raw_identity_requires_filter_all_query() {
        let raw = raw_object("check_suite_runs", "check-runs", serde_json::json!([1]));
        let raw_by_id = [(raw.raw_id.clone(), &raw)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let mut request = rest_request(&raw, "/repos/tailrocks/example/check-suites/9/check-runs");
        let mut request_by_id = [(request.request_id.clone(), &request)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert!(require_all_attempts_query(
            std::slice::from_ref(&raw.raw_id),
            &raw_by_id,
            &request_by_id,
            "check fixture",
        )
        .is_err());

        request.query_base64 = BASE64.encode(b"filter\x00all\x00per_page\x00100\x00");
        request_by_id = [(request.request_id.clone(), &request)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert!(require_all_attempts_query(
            std::slice::from_ref(&raw.raw_id),
            &raw_by_id,
            &request_by_id,
            "check fixture",
        )
        .is_ok());

        request.query_base64 = BASE64.encode(b"filter\x00latest\x00per_page\x00100\x00");
        request_by_id = [(request.request_id.clone(), &request)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert!(require_all_attempts_query(
            std::slice::from_ref(&raw.raw_id),
            &raw_by_id,
            &request_by_id,
            "check fixture",
        )
        .is_err());
    }
}
