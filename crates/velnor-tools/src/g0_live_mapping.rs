//! Strict mapping from the live GitHub observation ledger to the checker G0
//! contract.  This adapter is deliberately fail-closed: optional provider
//! fields, unresolved action revisions, missing rate/request identities, and
//! external checks without workflow associations are errors, never synthetic
//! IDs or successful empty rows.

use super::live_collector::{
    LiveCheck, LiveCollection, LiveDependency, LiveExecution, LivePullRequest,
    LivePullRequestIdentity, LiveRepository, LiveWorkflow,
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
    pub model_session: G0ModelSession,
    pub workload_artifact: G0ArtifactReference,
    /// Explicit workflow-to-workload mapping for repos with more than one
    /// reviewed workload.  A single-workload repo is mapped automatically.
    pub workflow_workloads: BTreeMap<(String, String), Vec<String>>,
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
    pub workflow_binding_checkout_shas: BTreeMap<String, String>,
    pub check_checkout_shas: BTreeMap<String, String>,
    pub pull_request_source_urls: BTreeMap<String, String>,
    pub graph_source: BTreeMap<String, (String, String)>,
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
    pub run_head_sha: String,
    pub name: String,
    pub digest: String,
    pub expired: Option<bool>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
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
        let session = &self.model_session;
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
        if session.raw_object_refs.is_empty() || self.workload_artifact.raw_object_refs.is_empty() {
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
        model_session: bindings.model_session.clone(),
        access: map_access(live, &raw_by_id)?,
        workload_artifact: bindings.workload_artifact.clone(),
    };
    let canonical = serde_json::to_vec(&snapshot).context("serialize canonical G0 snapshot")?;
    let evidence = G0InventoryEvidence {
        collector_snapshot: snapshot,
        collector_snapshot_sha256: sha256_digest(&canonical),
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
        media_type: raw.media_type.clone(),
        storage_ref: raw.storage_ref.clone(),
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
            safe_sha256: raw.sha256.clone(),
            safe_byte_length: raw.byte_length,
            safe_bytes_base64: raw.bytes_base64.clone(),
        })
        .collect();
    let mut workflow_binding_checkout_shas = BTreeMap::new();
    let mut check_checkout_shas = BTreeMap::new();
    let mut pull_request_source_urls = BTreeMap::new();
    let mut graph_source = BTreeMap::new();
    for repository in &live.repositories {
        for workflow in &repository.workflows {
            graph_source.insert(
                format!("workflow:{}:{}", repository.repository, workflow.path),
                (workflow.source_sha.clone(), workflow.path.clone()),
            );
        }
        for pull_request in &repository.open_prs {
            let pr_key = format!("{}#{}", repository.repository, pull_request.identity.number);
            pull_request_source_urls
                .insert(pr_key.clone(), pull_request.identity.source_url.clone());
            for binding in &pull_request.workflow_bindings {
                if let Some(checkout) = &binding.actual_checkout_sha {
                    workflow_binding_checkout_shas.insert(
                        format!("{pr_key}:{}:{}", binding.workflow_path, binding.event),
                        checkout.clone(),
                    );
                }
            }
            for check in &pull_request.checks {
                if let Some(checkout) = &check.actual_checkout_sha {
                    check_checkout_shas.insert(
                        format!("{pr_key}:{}:{}", check.context, check.check_run_id),
                        checkout.clone(),
                    );
                }
            }
        }
        for check in &repository.main_checks {
            if let Some(checkout) = &check.actual_checkout_sha {
                check_checkout_shas.insert(
                    format!(
                        "{}:{}:{}",
                        repository.repository, check.context, check.check_run_id
                    ),
                    checkout.clone(),
                );
            }
        }
    }
    G0MappingSupplement {
        canonical_snapshot_bytes_base64: BASE64.encode(canonical),
        request_payloads,
        raw_objects,
        artifacts,
        workflow_binding_checkout_shas,
        check_checkout_shas,
        pull_request_source_urls,
        graph_source,
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
            || !is_sha(&artifact.run_head_sha)
            || !is_sha256_digest(&artifact.digest)
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
    Ok(G0RepositoryInventory {
        repository: repository.repository.clone(),
        repository_id: repository.repository_id,
        default_branch: repository.default_branch.clone(),
        default_branch_sha: repository.default_branch_sha.clone(),
        rulesets,
        workflows,
        open_prs,
        main_checks,
        raw_object_refs: repository.raw_object_refs.clone(),
    })
}

fn map_workflow(
    workflow: &LiveWorkflow,
    repository: &LiveRepository,
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
    let source_raw = workflow
        .raw_object_refs
        .iter()
        .find_map(|raw_id| {
            raw_by_id
                .get(raw_id)
                .copied()
                .filter(|raw| raw.object_kind == "workflow.source")
        })
        .ok_or_else(|| anyhow!("workflow {} lacks source raw object", workflow.path))?;
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
    }
    let generated_state = G0ArtifactReference {
        name: workflow.path.clone(),
        schema: "github.workflow-source.v1".to_owned(),
        source_url: format!(
            "https://github.com/{}/blob/{}/{}",
            repository.repository, workflow.source_sha, workflow.path
        ),
        sha256: source_raw.sha256.clone(),
        observed_at_utc: observed_at_utc.to_owned(),
        raw_object_refs: workflow.raw_object_refs.clone(),
    };
    Ok(G0WorkflowInventory {
        path: workflow.path.clone(),
        revision: workflow.revision.clone(),
        source_sha: workflow.source_sha.clone(),
        events: workflow.events.clone(),
        reusable_workflows: workflow
            .reusable_workflows
            .iter()
            .map(map_dependency)
            .collect::<Result<Vec<_>>>()?,
        actions: workflow
            .actions
            .iter()
            .map(map_dependency)
            .collect::<Result<Vec<_>>>()?,
        scanners: workflow
            .scanners
            .iter()
            .map(map_dependency)
            .collect::<Result<Vec<_>>>()?,
        generated_state,
        raw_object_refs: workflow.raw_object_refs.clone(),
    })
}

fn map_dependency(dependency: &LiveDependency) -> Result<G0WorkflowDependency> {
    if !is_sha(&dependency.revision) {
        bail!(
            "workflow dependency {}/{} remains tag/ref-bound at {}",
            dependency.repository,
            dependency.path,
            dependency.revision
        );
    }
    Ok(G0WorkflowDependency {
        kind: dependency.kind.clone(),
        repository: dependency.repository.clone(),
        path: dependency.path.clone(),
        revision: dependency.revision.clone(),
        raw_object_refs: dependency.raw_object_refs.clone(),
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
            let _actual_checkout_sha = binding.actual_checkout_sha.clone().ok_or_else(|| {
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
        workflow_bindings,
        required_check_producers,
        raw_object_refs: pull_request.raw_object_refs.clone(),
    })
}

fn map_check(
    check: &LiveCheck,
    _workflows: &[LiveWorkflow],
    executions: Option<&[LiveExecution]>,
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
    let execution = executions
        .and_then(|runs| runs.iter().find(|run| run.run_id == workflow_run_id))
        .ok_or_else(|| anyhow!("check {} lacks concrete workflow execution", check.context))?;
    if execution.source_sha != check.source_sha {
        bail!(
            "check {} source SHA differs from workflow execution",
            check.context
        );
    }
    let job_id = check
        .job_id
        .ok_or_else(|| anyhow!("check {} lacks concrete job identity", check.context))?;
    if !execution.jobs.iter().any(|job| job.job_id == job_id) {
        bail!(
            "check {} job is not bound to workflow execution",
            check.context
        );
    }
    let _actual_checkout_sha = check
        .actual_checkout_sha
        .clone()
        .or_else(|| execution.actual_checkout_sha.clone())
        .ok_or_else(|| anyhow!("check {} lacks checkout identity", check.context))?;
    let run_attempt = check
        .run_attempt
        .or(Some(execution.run_attempt))
        .ok_or_else(|| anyhow!("check {} lacks run attempt", check.context))?;
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
        check_suite_id: check
            .check_suite_id
            .ok_or_else(|| anyhow!("check {} lacks suite identity", check.context))?,
        check_run_id: check.check_run_id,
        workflow_run_id,
        run_attempt,
        job_id,
        source_sha: check.source_sha.clone(),
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
        let source_raw_refs = workflow.raw_object_refs.clone();
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
                        raw_object_refs: dependency.raw_object_refs.clone(),
                    });
                }
                graph_raw_ids.extend(dependency.raw_object_refs.iter().cloned());
                edges.push(G0GraphEdge {
                    from: workload_node_id.clone(),
                    to: dependency_node_id,
                    kind: dependency.kind.clone(),
                    required: true,
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
