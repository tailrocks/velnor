//! Live GitHub facts collector.
//!
//! This layer performs the read-only traversal that the old `evidence_live`
//! snapshot collector did not retain: every page is acquired through the
//! provenance ledger, workflow attempts are expanded before jobs are read,
//! check suites are expanded before check runs are read, and run-scoped
//! artifacts are retained once per run.  It intentionally keeps optional provider
//! identities optional; the later G0 mapper must reject them rather than
//! inventing workflow/job associations for external checks.

use super::{
    collect_binary, collect_rest, github_check_suite_runs_request, github_check_suites_request,
    github_open_pull_requests_request, github_single_object_request,
    github_workflow_artifacts_request, github_workflow_attempt_jobs_request,
    github_workflow_attempt_request, github_workflow_runs_request, AcquisitionError,
    AcquisitionState, AuthIdentity, CollectionResult, IdentityReconciliation, RawObjectRef,
    RawObjectStore, RequestRecord, RestCollectionRequest, RevisionIdentity,
};
use crate::evidence_check::{ManifestDocument, ManifestRepository, CANONICAL_REPOSITORIES};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

/// Full PR identity retained at both sides of a live collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivePullRequestIdentity {
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub author_association: String,
    pub head_repository: Option<String>,
    pub head_ref: Option<String>,
    pub head_sha: String,
    pub base_repository: Option<String>,
    pub base_ref: String,
    pub base_sha: String,
    pub tested_merge_sha: Option<String>,
    pub merge_group_sha: Option<String>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

impl LivePullRequestIdentity {
    fn revision_identity(&self) -> RevisionIdentity {
        RevisionIdentity {
            repository: self.base_repository.clone().unwrap_or_default(),
            subject: format!("pr:{}", self.number),
            head_sha: Some(self.head_sha.clone()),
            base_sha: Some(self.base_sha.clone()),
            tested_merge_sha: self.tested_merge_sha.clone(),
            merge_group_sha: self.merge_group_sha.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveDependency {
    pub kind: String,
    pub repository: String,
    pub path: String,
    pub revision: String,
    pub resolved_path: Option<String>,
    pub source_url: Option<String>,
    pub source_bytes_base64: Option<String>,
    pub source_raw_object_refs: Vec<String>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveSourceJob {
    pub job_id: String,
    pub raw_object_refs: Vec<String>,
}

/// Checkout proof is distinct from the provider's run/job `head_sha`.
/// GitHub's API response binds the source revision, but does not attest what
/// the runner actually checked out.  The collector therefore retains the API
/// observation and raw refs while leaving `proof` absent until a separately
/// captured, run/job/attempt-bound attestation is available.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCheckoutObservation {
    pub api_head_sha: String,
    pub api_raw_object_refs: Vec<String>,
    pub(crate) proof: Option<LiveCheckoutProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveCheckoutProof {
    checkout_sha: String,
    source_kind: String,
    raw_object_refs: Vec<String>,
}

impl LiveCheckoutProof {
    pub(crate) fn raw_object_refs(&self) -> &[String] {
        &self.raw_object_refs
    }
}

impl LiveCheckoutObservation {
    fn api_head_only(api_head_sha: String, api_raw_object_refs: Vec<String>) -> Self {
        Self {
            api_head_sha,
            api_raw_object_refs,
            proof: None,
        }
    }

    pub(crate) fn actual_checkout_sha(&self) -> Option<&str> {
        self.proof.as_ref().map(|proof| proof.checkout_sha.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveWorkflow {
    pub path: String,
    pub revision: String,
    pub source_sha: String,
    pub source_url: String,
    pub source_bytes_base64: String,
    pub source_raw_object_refs: Vec<String>,
    pub events: Vec<String>,
    pub source_jobs: Vec<LiveSourceJob>,
    pub reusable_workflows: Vec<LiveDependency>,
    pub actions: Vec<LiveDependency>,
    pub scanners: Vec<LiveDependency>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRulesetCheck {
    pub context: String,
    pub app_id: Option<String>,
    pub ruleset_id: u64,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRuleset {
    pub ruleset_id: u64,
    pub name: String,
    pub source_url: String,
    pub complete: bool,
    pub required_checks: Vec<LiveRulesetCheck>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveJob {
    pub job_id: u64,
    /// Exact check-run identity exposed by the Actions jobs API.  This is
    /// required for binding a provider check to one concrete job.
    pub check_run_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub event: String,
    pub source_sha: Option<String>,
    pub checkout: LiveCheckoutObservation,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveArtifact {
    pub artifact_id: u64,
    pub run_id: u64,
    /// The list-artifacts API is run-scoped and does not expose run_attempt.
    /// Keep that fact explicit instead of stamping the latest attempt.
    pub run_attempt: Option<u32>,
    pub run_head_sha: String,
    pub name: String,
    pub digest: String,
    pub expired: Option<bool>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveExecution {
    pub run_id: u64,
    pub run_attempt: u32,
    pub workflow_path: String,
    pub workflow_revision: String,
    pub event: String,
    pub source_sha: String,
    pub checkout: LiveCheckoutObservation,
    pub status: String,
    pub conclusion: Option<String>,
    pub source_url: String,
    pub jobs: Vec<LiveJob>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCheck {
    pub context: String,
    pub app_id: Option<String>,
    pub app_slug: String,
    pub check_suite_id: Option<u64>,
    pub check_run_id: u64,
    pub workflow_run_id: Option<u64>,
    pub job_id: Option<u64>,
    pub run_attempt: Option<u32>,
    pub source_sha: String,
    pub checkout: LiveCheckoutObservation,
    pub event: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivePullRequest {
    pub identity: LivePullRequestIdentity,
    pub workflow_bindings: Vec<LiveWorkflowBinding>,
    pub executions: Vec<LiveExecution>,
    pub checks: Vec<LiveCheck>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveWorkflowBinding {
    pub workflow_path: String,
    pub workflow_revision: String,
    pub event: String,
    pub source_sha: String,
    pub checkout: LiveCheckoutObservation,
    pub run_ids: Vec<u64>,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRepository {
    pub repository: String,
    pub repository_id: u64,
    pub default_branch: String,
    pub default_branch_sha: String,
    pub rulesets: Vec<LiveRuleset>,
    pub workflows: Vec<LiveWorkflow>,
    pub open_prs: Vec<LivePullRequest>,
    pub artifacts: Vec<LiveArtifact>,
    pub main_executions: Vec<LiveExecution>,
    pub main_checks: Vec<LiveCheck>,
    pub closing_repository_id: u64,
    pub closing_default_branch: String,
    pub closing_default_branch_sha: String,
    pub source_invalidated: bool,
    pub access_state: String,
    pub access_gaps: Vec<String>,
    pub raw_object_refs: Vec<String>,
}

/// Durable result of a complete read-only collection.  `requests` and
/// `raw_objects` are never reduced to summary counts; nested observations keep
/// their exact raw-object IDs for checker-side binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCollection {
    pub schema_version: u32,
    pub manifest_id: String,
    pub snapshot_id: String,
    pub observed_at_utc: String,
    pub completed_at_utc: String,
    pub auth: AuthIdentity,
    pub requests: Vec<RequestRecord>,
    pub raw_objects: Vec<RawObjectRef>,
    pub repositories: Vec<LiveRepository>,
    pub opening_prs: Vec<LivePullRequestIdentity>,
    pub closing_prs: Vec<LivePullRequestIdentity>,
    pub reconciliation: IdentityReconciliation,
}

/// Small real-API capture used to verify credential resolution, endpoint
/// binding, pagination/provenance, and raw-byte persistence before a 32-repo
/// run.  It is intentionally not shaped as G0 evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveSampleCollection {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub observed_at_utc: String,
    pub completed_at_utc: String,
    pub repository: String,
    pub auth: AuthIdentity,
    pub repository_object: Value,
    pub default_branch: String,
    pub default_branch_commit: Value,
    pub requests: Vec<RequestRecord>,
    pub raw_objects: Vec<RawObjectRef>,
}

/// Optional append-only sink for request progress.  The sink receives only
/// typed request metadata; response bytes and credential material stay in the
/// raw-object store/transport boundary.
pub trait LiveProgressSink {
    fn record_request(&mut self, request: &RequestRecord) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyTreeKey {
    endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DependencySourceKey {
    metadata_endpoint: String,
    raw_endpoint: String,
}

#[derive(Debug, Clone)]
struct CachedDependencySource {
    source_url: String,
    source_text: String,
    source_raw_object_refs: Vec<String>,
}

#[derive(Default)]
struct Ledger<'a> {
    requests: Vec<RequestRecord>,
    raw_objects: Vec<RawObjectRef>,
    request_ids: BTreeSet<String>,
    raw_ids: BTreeSet<String>,
    dependency_trees: BTreeMap<DependencyTreeKey, (Value, Vec<String>)>,
    dependency_sources: BTreeMap<DependencySourceKey, CachedDependencySource>,
    progress: Option<&'a mut dyn LiveProgressSink>,
}

impl<'a> Ledger<'a> {
    fn with_progress(progress: Option<&'a mut dyn LiveProgressSink>) -> Self {
        Self {
            progress,
            ..Self::default()
        }
    }

    fn record_progress(&mut self, requests: &[RequestRecord]) -> Result<()> {
        if let Some(progress) = self.progress.as_deref_mut() {
            for request in requests {
                progress.record_request(request)?;
            }
        }
        Ok(())
    }

    fn ingest(&mut self, result: CollectionResult) -> Result<Vec<Value>> {
        self.record_progress(&result.requests)?;
        let mut request_ids = BTreeSet::new();
        for request in &result.requests {
            if !request_ids.insert(request.request_id.clone())
                || self.request_ids.contains(&request.request_id)
            {
                bail!("duplicate acquisition request id {}", request.request_id);
            }
        }
        let mut raw_ids = BTreeSet::new();
        for raw in &result.raw_objects {
            if !raw_ids.insert(raw.raw_id.clone()) || self.raw_ids.contains(&raw.raw_id) {
                bail!("duplicate acquisition raw id {}", raw.raw_id);
            }
        }
        self.request_ids.extend(request_ids);
        self.raw_ids.extend(raw_ids);
        self.requests.extend(result.requests);
        self.raw_objects.extend(result.raw_objects);
        if !result.complete {
            bail!(
                "GitHub collection page set is incomplete: {:?}",
                result.state
            );
        }
        Ok(result.items)
    }

    fn raw_ids_since(&self, start: usize) -> Vec<String> {
        self.raw_objects[start..]
            .iter()
            .map(|raw| raw.raw_id.clone())
            .collect()
    }

    fn raw_ids_for(result: &CollectionResult) -> Vec<String> {
        result
            .raw_objects
            .iter()
            .map(|raw| raw.raw_id.clone())
            .collect()
    }
}

/// Collect the complete GitHub inventory for every repository in the reviewed
/// manifest.  The caller supplies an auth identity with the token registered
/// in its private credential registry; the convenience CLI wrapper below does
/// that binding for the concrete transport.
fn validate_manifest_scope(manifest: &ManifestDocument, snapshot_id: &str) -> Result<()> {
    if snapshot_id.trim().is_empty()
        || manifest.schema_version != 2
        || manifest.manifest_id != "github-first-dual-lane-2026-09-19"
        || manifest.source.repository != "tailrocks/velnor"
        || manifest.repositories.len() != CANONICAL_REPOSITORIES.len()
    {
        bail!("live collector requires the reviewed 32-repository manifest");
    }
    if !is_hex_revision(&manifest.source.revision, 40)
        || !is_digest(&manifest.source.digest)
        || manifest.source.reviewed_by.trim().is_empty()
    {
        bail!("live collector manifest source identity is malformed");
    }
    let mut seen = BTreeSet::new();
    for repository in &manifest.repositories {
        if !seen.insert(repository.repository.as_str()) {
            bail!("manifest repeats repository {}", repository.repository);
        }
        if !CANONICAL_REPOSITORIES.contains(&repository.repository.as_str()) {
            bail!(
                "manifest contains repository outside reviewed scope: {}",
                repository.repository
            );
        }
        if repository.default_branch.trim().is_empty()
            || repository.workflow_path.trim().is_empty()
            || !is_hex_revision(&repository.workflow_revision, 40)
        {
            bail!(
                "manifest repository {} has malformed workflow identity",
                repository.repository
            );
        }
    }
    let expected = CANONICAL_REPOSITORIES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if seen != expected {
        bail!("manifest repository set does not exactly match reviewed scope");
    }
    Ok(())
}

fn is_hex_revision(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| is_hex_revision(hex, 64))
}

pub async fn collect_live<T, S>(
    transport: &T,
    store: &mut S,
    auth: AuthIdentity,
    manifest: &ManifestDocument,
    snapshot_id: impl Into<String>,
) -> Result<LiveCollection>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    collect_live_with_progress(transport, store, auth, manifest, snapshot_id, None).await
}

/// Collect the complete GitHub inventory and append each committed request to
/// an optional progress sink.  The legacy-shaped wrapper above intentionally
/// remains a no-sink API for callers that only need the in-memory result.
pub async fn collect_live_with_progress<T, S>(
    transport: &T,
    store: &mut S,
    mut auth: AuthIdentity,
    manifest: &ManifestDocument,
    snapshot_id: impl Into<String>,
    progress: Option<&mut dyn LiveProgressSink>,
) -> Result<LiveCollection>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let snapshot_id = snapshot_id.into();
    validate_manifest_scope(manifest, &snapshot_id)?;
    let observed_at_utc = utc_now();
    let mut ledger = Ledger::with_progress(progress);
    let viewer_result = collect_result(
        transport,
        store,
        &auth,
        github_single_object_request("auth--viewer", "/user", "auth.viewer"),
    )
    .await?;
    let viewer = viewer_result
        .items
        .first()
        .ok_or_else(|| anyhow!("GitHub /user returned no object"))?
        .clone();
    let viewer_id = required_identifier(&viewer, &["id"])?;
    let viewer_login = required_string(&viewer, &["login"])?;
    let viewer_scopes = viewer_result
        .requests
        .iter()
        .find_map(|request| request.safe_scopes.clone())
        .ok_or_else(|| anyhow!("GitHub /user did not expose safe OAuth scopes"))?;
    ledger.ingest(viewer_result)?;
    auth.viewer_id = Some(viewer_id);
    auth.viewer_login = Some(viewer_login);
    auth.safe_scopes = viewer_scopes.into_iter().collect();

    let mut opening_prs = Vec::new();
    for repository in &manifest.repositories {
        opening_prs.extend(
            collect_pr_identities(transport, store, &auth, repository, "opening", &mut ledger)
                .await
                .with_context(|| format!("opening PR census for {}", repository.repository))?,
        );
    }

    let mut repositories = Vec::with_capacity(manifest.repositories.len());
    for repository in &manifest.repositories {
        repositories.push(
            collect_repository(transport, store, &auth, repository, &mut ledger)
                .await
                .with_context(|| format!("live repository facts for {}", repository.repository))?,
        );
    }

    for live_repository in &mut repositories {
        let manifest_repository = manifest
            .repositories
            .iter()
            .find(|repository| repository.repository == live_repository.repository)
            .ok_or_else(|| anyhow!("missing manifest entry for {}", live_repository.repository))?;
        let (closing_id, closing_branch, closing_sha, raw_ids) = collect_repository_tip(
            transport,
            store,
            &auth,
            manifest_repository,
            "closing",
            &mut ledger,
        )
        .await
        .with_context(|| format!("closing repository tip for {}", live_repository.repository))?;
        live_repository.closing_repository_id = closing_id;
        live_repository.closing_default_branch = closing_branch.clone();
        live_repository.closing_default_branch_sha = closing_sha.clone();
        live_repository.source_invalidated = live_repository.repository_id != closing_id
            || live_repository.default_branch != closing_branch
            || live_repository.default_branch_sha != closing_sha;
        live_repository.raw_object_refs.extend(raw_ids);
        live_repository.raw_object_refs.sort();
        live_repository.raw_object_refs.dedup();
    }

    let mut closing_prs = Vec::new();
    for repository in &manifest.repositories {
        closing_prs.extend(
            collect_pr_identities(transport, store, &auth, repository, "closing", &mut ledger)
                .await
                .with_context(|| format!("closing PR census for {}", repository.repository))?,
        );
    }
    let reconciliation = super::reconcile_identity_sets(
        opening_prs
            .iter()
            .map(LivePullRequestIdentity::revision_identity)
            .collect(),
        closing_prs
            .iter()
            .map(LivePullRequestIdentity::revision_identity)
            .collect(),
    );
    Ok(LiveCollection {
        schema_version: 1,
        manifest_id: manifest.manifest_id.clone(),
        snapshot_id,
        observed_at_utc,
        completed_at_utc: utc_now(),
        auth,
        requests: ledger.requests,
        raw_objects: ledger.raw_objects,
        repositories,
        opening_prs,
        closing_prs,
        reconciliation,
    })
}

/// Capture a bounded set of real GitHub objects for transport/store
/// verification.  The sample includes authenticated viewer identity, one
/// repository object, and its default-branch commit; every response remains
/// in the same request/raw ledger as the full collector.
pub async fn collect_live_sample<T, S>(
    transport: &T,
    store: &mut S,
    mut auth: AuthIdentity,
    repository: &str,
    snapshot_id: impl Into<String>,
) -> Result<LiveSampleCollection>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    validate_repository_slug(repository)?;
    let snapshot_id = snapshot_id.into();
    if snapshot_id.trim().is_empty() {
        bail!("sample snapshot ID must be non-empty");
    }
    let observed_at_utc = utc_now();
    let mut ledger = Ledger::default();
    let viewer_result = collect_result(
        transport,
        store,
        &auth,
        github_single_object_request("sample-auth-viewer", "/user", "auth.viewer"),
    )
    .await?;
    let viewer = viewer_result
        .items
        .first()
        .ok_or_else(|| anyhow!("GitHub /user returned no object"))?
        .clone();
    let viewer_id = required_identifier(&viewer, &["id"])?;
    let viewer_login = required_string(&viewer, &["login"])?;
    let viewer_scopes = viewer_result
        .requests
        .iter()
        .find_map(|request| request.safe_scopes.clone())
        .ok_or_else(|| anyhow!("GitHub /user did not expose safe OAuth scopes"))?;
    ledger.ingest(viewer_result)?;
    auth.viewer_id = Some(viewer_id);
    auth.viewer_login = Some(viewer_login);
    auth.safe_scopes = viewer_scopes.into_iter().collect();

    let (repository_object, _) = collect_one(
        transport,
        store,
        &auth,
        &mut ledger,
        github_single_object_request(
            "sample-repository",
            format!("/repos/{repository}"),
            "repository",
        ),
    )
    .await?;
    let default_branch = required_string(&repository_object, &["default_branch"])?;
    let (default_branch_commit, _) = collect_one(
        transport,
        store,
        &auth,
        &mut ledger,
        github_single_object_request(
            "sample-default-commit",
            format!("/repos/{repository}/commits/{default_branch}"),
            "default_branch.commit",
        ),
    )
    .await?;
    Ok(LiveSampleCollection {
        schema_version: 1,
        snapshot_id,
        observed_at_utc,
        completed_at_utc: utc_now(),
        repository: repository.to_owned(),
        auth,
        repository_object,
        default_branch,
        default_branch_commit,
        requests: ledger.requests,
        raw_objects: ledger.raw_objects,
    })
}

async fn collect_result<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    request: RestCollectionRequest,
) -> Result<CollectionResult>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    collect_rest(transport, store, auth, request)
        .await
        .map_err(|error| anyhow!(acquisition_error_message(error)))
}

async fn collect_items<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    ledger: &mut Ledger<'_>,
    request: RestCollectionRequest,
) -> Result<(Vec<Value>, Vec<String>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let result = collect_result(transport, store, auth, request).await?;
    let raw_ids = Ledger::raw_ids_for(&result);
    let items = ledger.ingest(result)?;
    Ok((items, raw_ids))
}

async fn collect_one<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    ledger: &mut Ledger<'_>,
    request: RestCollectionRequest,
) -> Result<(Value, Vec<String>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let singleton = request.item_field.as_deref() == Some("$object");
    let (items, raw_ids) = collect_items(transport, store, auth, ledger, request).await?;
    if singleton && items.len() > 1 {
        let first_identity = singleton_identity(&items[0])
            .ok_or_else(|| anyhow!("singleton endpoint returned unidentifiable pages"))?;
        if items
            .iter()
            .skip(1)
            .any(|item| singleton_identity(item).as_deref() != Some(first_identity.as_str()))
        {
            bail!("singleton endpoint returned conflicting object pages");
        }
        return Ok((items[0].clone(), raw_ids));
    }
    if items.len() != 1 {
        bail!("single-object endpoint returned {} objects", items.len());
    }
    let Some(item) = items.into_iter().next() else {
        bail!("single-object endpoint returned no object");
    };
    Ok((item, raw_ids))
}

/// Fetch a contents endpoint in GitHub's raw media representation.  The
/// metadata request and this raw request are both retained; callers compare
/// their bytes before treating the source as immutable YAML evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "source acquisition keeps transport, storage, auth, ledger, and source identity explicit"
)]
async fn collect_raw_source<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    ledger: &mut Ledger<'_>,
    collection_id: String,
    repository: &str,
    path: &str,
    revision: &str,
    object_kind: &str,
) -> Result<(String, Vec<String>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let encoded_path = encode_api_path(path);
    let result = super::collect_binary(
        transport,
        store,
        auth,
        super::RestCollectionRequest::new(
            collection_id,
            format!("/repos/{repository}/contents/{encoded_path}?ref={revision}"),
            None::<String>,
            object_kind,
        )
        .with_accept("application/vnd.github.raw+json")
        .with_per_page(1),
    )
    .await
    .map_err(|error| anyhow!(acquisition_error_message(error)))?;
    // Snapshot the response metadata before ingest consumes the collection.
    // Ingest before inspecting success.  A failed raw-source request is still
    // evidence: its typed request state, error response, and any prior pages
    // must remain in the ledger for the caller's failure report.
    let raw = result
        .raw_objects
        .iter()
        .find(|raw| raw.object_kind == object_kind)
        .cloned();
    let raw_ids = result
        .raw_objects
        .iter()
        .map(|raw| raw.raw_id.clone())
        .collect::<Vec<_>>();
    ledger.ingest(result)?;
    let raw = raw.ok_or_else(|| anyhow!("raw source response lacks object kind {object_kind}"))?;
    let bytes = BASE64
        .decode(&raw.bytes_base64)
        .context("decode raw workflow source bytes")?;
    let source = String::from_utf8(bytes).context("raw workflow source is not UTF-8")?;
    Ok((source, raw_ids))
}

fn singleton_identity(value: &Value) -> Option<String> {
    ["id", "node_id", "sha", "url", "path"]
        .into_iter()
        .find_map(|field| value.get(field).map(|value| value.to_string()))
}

async fn collect_pr_identities<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    repository: &ManifestRepository,
    phase: &str,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<LivePullRequestIdentity>>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (values, list_raw_ids) = collect_items(
        transport,
        store,
        auth,
        ledger,
        github_open_pull_requests_request(
            collection_id(&repository.repository, &format!("{phase}-open-prs")),
            &repository.repository,
        ),
    )
    .await?;
    let mut identities = Vec::with_capacity(values.len());
    let mut seen_numbers = BTreeSet::new();
    for value in values {
        let number = required_u64(&value, &["number"])?;
        if !seen_numbers.insert(number) {
            bail!(
                "{} returned pull request {number} more than once",
                repository.repository
            );
        }
        let (detail, detail_raw_ids) = collect_one(
            transport,
            store,
            auth,
            ledger,
            github_single_object_request(
                collection_id(&repository.repository, &format!("{phase}-pr-{number}")),
                format!("/repos/{}/pulls/{number}", repository.repository),
                "pull_request",
            ),
        )
        .await?;
        let mut raw_ids = list_raw_ids.clone();
        raw_ids.extend(detail_raw_ids);
        let identity = parse_pull_request_identity(&detail, raw_ids)?;
        if identity.number != number {
            bail!(
                "pull request detail returned #{} for requested #{}",
                identity.number,
                number
            );
        }
        validate_pull_request_identity(&identity, &repository.repository)?;
        identities.push(identity);
    }
    Ok(identities)
}

async fn collect_repository_tip<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    phase: &str,
    ledger: &mut Ledger<'_>,
) -> Result<(u64, String, String, Vec<String>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (repository_value, repository_raw_ids) = collect_one(
        transport,
        store,
        auth,
        ledger,
        github_single_object_request(
            collection_id(&manifest.repository, &format!("{phase}-repository")),
            format!("/repos/{}", manifest.repository),
            format!("{phase}.repository"),
        ),
    )
    .await?;
    let full_name = required_string(&repository_value, &["full_name"])?;
    if full_name != manifest.repository {
        bail!(
            "repository endpoint returned {full_name}, expected {}",
            manifest.repository
        );
    }
    let repository_id = required_u64(&repository_value, &["id"])?;
    let default_branch = required_string(&repository_value, &["default_branch"])?;
    let (commit, commit_raw_ids) = collect_one(
        transport,
        store,
        auth,
        ledger,
        github_single_object_request(
            collection_id(&manifest.repository, &format!("{phase}-default-commit")),
            format!("/repos/{}/commits/{default_branch}", manifest.repository),
            format!("{phase}.default_branch.commit"),
        ),
    )
    .await?;
    let sha = required_sha(&commit, &["sha"])?;
    let mut raw_ids = repository_raw_ids;
    raw_ids.extend(commit_raw_ids);
    raw_ids.sort();
    raw_ids.dedup();
    Ok((repository_id, default_branch, sha, raw_ids))
}

async fn collect_repository<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    ledger: &mut Ledger<'_>,
) -> Result<LiveRepository>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let raw_start = ledger.raw_objects.len();
    let request_start = ledger.requests.len();
    let (repository_value, _) = collect_one(
        transport,
        store,
        auth,
        ledger,
        github_single_object_request(
            collection_id(&manifest.repository, "repository"),
            format!("/repos/{}", manifest.repository),
            "repository",
        ),
    )
    .await?;
    let full_name = required_string(&repository_value, &["full_name"])?;
    if full_name != manifest.repository {
        bail!(
            "repository endpoint returned {full_name}, expected {}",
            manifest.repository
        );
    }
    let repository_url = required_string(&repository_value, &["html_url"])?;
    validate_repository_url(&repository_url, &manifest.repository)?;
    let repository_id = required_u64(&repository_value, &["id"])?;
    let default_branch = required_string(&repository_value, &["default_branch"])?;
    if default_branch != manifest.default_branch {
        bail!(
            "{} default branch changed from reviewed {} to {}",
            manifest.repository,
            manifest.default_branch,
            default_branch
        );
    }
    let (commit, _) = collect_one(
        transport,
        store,
        auth,
        ledger,
        github_single_object_request(
            collection_id(&manifest.repository, "default-commit"),
            format!("/repos/{}/commits/{}", manifest.repository, default_branch),
            "default_branch.commit",
        ),
    )
    .await?;
    let default_branch_sha = required_sha(&commit, &["sha"])?;
    let rulesets = collect_rulesets(transport, store, auth, manifest, ledger).await?;
    let identities =
        collect_pr_identities(transport, store, auth, manifest, "inventory", ledger).await?;
    let mut workflows = collect_workflows(
        transport,
        store,
        auth,
        manifest,
        &default_branch_sha,
        ledger,
    )
    .await?;
    let mut open_prs = Vec::with_capacity(identities.len());
    let mut artifacts = Vec::new();
    for identity in identities {
        let (executions, checks, run_artifacts) = collect_pr_execution_facts(
            transport,
            store,
            auth,
            manifest,
            repository_id,
            &mut workflows,
            &identity,
            ledger,
        )
        .await?;
        merge_artifacts(&mut artifacts, run_artifacts)?;
        let workflow_bindings = workflow_bindings(&executions)?;
        let raw_ids = identity.raw_object_refs.clone();
        open_prs.push(LivePullRequest {
            identity,
            workflow_bindings,
            executions,
            checks,
            raw_object_refs: raw_ids,
        });
    }
    let (main_executions, main_artifacts) = collect_executions_for_source(
        transport,
        store,
        auth,
        manifest,
        repository_id,
        &mut workflows,
        &default_branch_sha,
        "main",
        ledger,
    )
    .await?;
    merge_artifacts(&mut artifacts, main_artifacts)?;
    let main_checks = collect_check_facts(
        transport,
        store,
        auth,
        manifest,
        &default_branch_sha,
        "main",
        ledger,
    )
    .await?;
    let mut access_gaps = access_gaps(&repository_value);
    access_gaps.extend(access_endpoint_gaps(
        &ledger.requests[request_start..],
        &manifest.repository,
    ));
    access_gaps.sort();
    access_gaps.dedup();
    let access_state = if access_gaps.is_empty() {
        "observed".to_owned()
    } else {
        "unknown".to_owned()
    };
    Ok(LiveRepository {
        repository: manifest.repository.clone(),
        repository_id,
        default_branch: default_branch.clone(),
        default_branch_sha: default_branch_sha.clone(),
        rulesets,
        workflows,
        open_prs,
        artifacts,
        main_executions,
        main_checks,
        closing_repository_id: repository_id,
        closing_default_branch: default_branch.clone(),
        closing_default_branch_sha: default_branch_sha.clone(),
        source_invalidated: false,
        access_state,
        access_gaps,
        raw_object_refs: ledger.raw_ids_since(raw_start),
    })
}

async fn collect_rulesets<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<LiveRuleset>>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (values, list_raw_ids) = collect_items(
        transport,
        store,
        auth,
        ledger,
        super::RestCollectionRequest::new(
            collection_id(&manifest.repository, "rulesets"),
            format!("/repos/{}/rulesets", manifest.repository),
            None::<String>,
            "rulesets",
        )
        .with_query("includes_parents", "true"),
    )
    .await?;
    let mut rulesets = Vec::with_capacity(values.len());
    let mut seen_ids = BTreeSet::new();
    for value in values {
        let id = required_u64(&value, &["id"])?;
        if !seen_ids.insert(id) {
            bail!("ruleset {id} was observed more than once");
        }
        let name = required_string(&value, &["name"])?;
        let (detail, detail_raw_ids) = collect_one(
            transport,
            store,
            auth,
            ledger,
            github_single_object_request(
                collection_id(&manifest.repository, &format!("ruleset-{id}")),
                format!("/repos/{}/rulesets/{id}", manifest.repository),
                "ruleset",
            ),
        )
        .await?;
        let mut raw_ids = list_raw_ids.clone();
        raw_ids.extend(detail_raw_ids.clone());
        let required_checks = parse_ruleset_checks(&detail, id, raw_ids.clone())?;
        let complete = ruleset_is_complete(&detail);
        rulesets.push(LiveRuleset {
            ruleset_id: id,
            name,
            source_url: format!(
                "https://api.github.com/repos/{}/rulesets/{id}",
                manifest.repository
            ),
            complete,
            required_checks,
            raw_object_refs: raw_ids,
        });
    }
    Ok(rulesets)
}

async fn collect_workflows<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    source_sha: &str,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<LiveWorkflow>>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (values, list_raw_ids) = collect_items(
        transport,
        store,
        auth,
        ledger,
        super::RestCollectionRequest::new(
            collection_id(&manifest.repository, "workflows"),
            format!("/repos/{}/actions/workflows", manifest.repository),
            Some("workflows"),
            "workflows",
        ),
    )
    .await?;
    if values.is_empty() {
        bail!("{} returned no workflows", manifest.repository);
    }
    let mut workflows = Vec::with_capacity(values.len());
    let mut seen_paths = BTreeSet::new();
    for value in values {
        let path = required_string(&value, &["path"])?;
        if !seen_paths.insert(path.clone()) {
            bail!("workflow path {path} was observed more than once");
        }
        let encoded_path = encode_api_path(&path);
        let (content, content_raw_ids) = collect_one(
            transport,
            store,
            auth,
            ledger,
            github_single_object_request(
                collection_id(
                    &manifest.repository,
                    &format!("workflow-content-{}", safe_id(&path)),
                ),
                format!(
                    "/repos/{}/contents/{encoded_path}?ref={source_sha}",
                    manifest.repository
                ),
                "workflow.source",
            ),
        )
        .await?;
        validate_workflow_content(&content, &manifest.repository, &path, source_sha)?;
        let source_url = required_string(&content, &["url"])?;
        let revision = required_sha(&content, &["sha"])?;
        let metadata_source_text = decode_workflow_content(&content)?;
        let (source_text, source_raw_ids) = collect_raw_source(
            transport,
            store,
            auth,
            ledger,
            collection_id(
                &manifest.repository,
                &format!("workflow-raw-source-{}", safe_id(&path)),
            ),
            &manifest.repository,
            &path,
            source_sha,
            "workflow.source",
        )
        .await?;
        if source_text != metadata_source_text {
            bail!("workflow source metadata and raw media bytes differ for {path}");
        }
        let events = parse_workflow_events(&source_text)?;
        let source_jobs = parse_source_jobs(&source_text, manifest, &source_raw_ids)?;
        let (reusable_workflows, actions, scanners) = parse_workflow_dependencies(
            &source_text,
            &manifest.repository,
            source_sha,
            &source_raw_ids,
        )?;
        let dependencies = bind_workflow_dependencies(
            WorkflowDependencyContext {
                transport,
                store,
                auth,
                manifest,
                workflow_path: &path,
                source_sha,
                ledger,
            },
            WorkflowDependencyGroups {
                reusable: reusable_workflows,
                actions,
                scanners,
            },
        )
        .await?;
        let WorkflowDependencyGroups {
            reusable: reusable_workflows,
            actions,
            scanners,
        } = dependencies;
        let mut raw_ids = list_raw_ids.clone();
        raw_ids.extend(content_raw_ids.clone());
        raw_ids.extend(source_raw_ids.clone());
        workflows.push(LiveWorkflow {
            path,
            revision,
            source_sha: source_sha.to_owned(),
            source_url,
            source_bytes_base64: BASE64.encode(source_text.as_bytes()),
            source_raw_object_refs: source_raw_ids,
            events,
            source_jobs,
            reusable_workflows,
            actions,
            scanners,
            raw_object_refs: raw_ids,
        });
    }
    Ok(workflows)
}

async fn collect_workflow_at_source<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    path: &str,
    source_sha: &str,
    ledger: &mut Ledger<'_>,
) -> Result<LiveWorkflow>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let encoded_path = encode_api_path(path);
    let (content, content_raw_ids) = collect_one(
        transport,
        store,
        auth,
        ledger,
        github_single_object_request(
            collection_id(
                &manifest.repository,
                &format!("workflow-content-{}-{}", safe_id(path), safe_id(source_sha)),
            ),
            format!(
                "/repos/{}/contents/{encoded_path}?ref={source_sha}",
                manifest.repository
            ),
            "workflow.source",
        ),
    )
    .await?;
    validate_workflow_content(&content, &manifest.repository, path, source_sha)?;
    let source_url = required_string(&content, &["url"])?;
    let revision = required_sha(&content, &["sha"])?;
    let metadata_source_text = decode_workflow_content(&content)?;
    let (source_text, source_raw_ids) = collect_raw_source(
        transport,
        store,
        auth,
        ledger,
        collection_id(
            &manifest.repository,
            &format!(
                "workflow-raw-source-{}-{}",
                safe_id(path),
                safe_id(source_sha)
            ),
        ),
        &manifest.repository,
        path,
        source_sha,
        "workflow.source",
    )
    .await?;
    if source_text != metadata_source_text {
        bail!("workflow source metadata and raw media bytes differ for {path}");
    }
    let source_jobs = parse_source_jobs(&source_text, manifest, &source_raw_ids)?;
    let (reusable_workflows, actions, scanners) = parse_workflow_dependencies(
        &source_text,
        &manifest.repository,
        source_sha,
        &source_raw_ids,
    )?;
    let dependencies = bind_workflow_dependencies(
        WorkflowDependencyContext {
            transport,
            store,
            auth,
            manifest,
            workflow_path: path,
            source_sha,
            ledger,
        },
        WorkflowDependencyGroups {
            reusable: reusable_workflows,
            actions,
            scanners,
        },
    )
    .await?;
    let WorkflowDependencyGroups {
        reusable: reusable_workflows,
        actions,
        scanners,
    } = dependencies;
    Ok(LiveWorkflow {
        path: path.to_owned(),
        revision,
        source_sha: source_sha.to_owned(),
        source_url,
        source_bytes_base64: BASE64.encode(source_text.as_bytes()),
        source_raw_object_refs: source_raw_ids.clone(),
        events: parse_workflow_events(&source_text)?,
        source_jobs,
        reusable_workflows,
        actions,
        scanners,
        raw_object_refs: content_raw_ids.into_iter().chain(source_raw_ids).collect(),
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the collector keeps transport, storage, auth, repository, workflow, identity, and ledger boundaries explicit"
)]
async fn collect_pr_execution_facts<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    repository_id: u64,
    workflows: &mut Vec<LiveWorkflow>,
    identity: &LivePullRequestIdentity,
    ledger: &mut Ledger<'_>,
) -> Result<(Vec<LiveExecution>, Vec<LiveCheck>, Vec<LiveArtifact>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let mut sources = vec![identity.head_sha.clone(), identity.base_sha.clone()];
    if let Some(merge) = &identity.tested_merge_sha {
        sources.push(merge.clone());
    }
    sources.sort();
    sources.dedup();
    let mut executions = Vec::new();
    let mut artifacts = Vec::new();
    for source_sha in sources {
        let (source_executions, source_artifacts) = collect_executions_for_source(
            transport,
            store,
            auth,
            manifest,
            repository_id,
            workflows,
            &source_sha,
            &format!("pr-{}", identity.number),
            ledger,
        )
        .await?;
        executions.extend(source_executions);
        artifacts.extend(source_artifacts);
    }
    let source_sha = identity
        .tested_merge_sha
        .as_deref()
        .ok_or_else(|| anyhow!("PR #{} lacks tested merge SHA", identity.number))?;
    let checks = collect_check_facts(
        transport,
        store,
        auth,
        manifest,
        source_sha,
        &format!("pr-{}", identity.number),
        ledger,
    )
    .await?;
    Ok((executions, checks, artifacts))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the collector keeps transport, storage, auth, manifest, workflow, source, and ledger boundaries explicit"
)]
async fn collect_executions_for_source<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    repository_id: u64,
    workflows: &mut Vec<LiveWorkflow>,
    source_sha: &str,
    collection_prefix: &str,
    ledger: &mut Ledger<'_>,
) -> Result<(Vec<LiveExecution>, Vec<LiveArtifact>)>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (runs, run_list_raw_ids) = collect_items(
        transport,
        store,
        auth,
        ledger,
        github_workflow_runs_request(
            collection_id(
                &manifest.repository,
                &format!("{collection_prefix}-runs-{source_sha}"),
            ),
            &manifest.repository,
        )
        .with_query("head_sha", source_sha),
    )
    .await?;
    let mut executions = Vec::new();
    let mut artifacts = Vec::new();
    let mut seen_runs = BTreeSet::new();
    for run in runs {
        let run_id = required_u64(&run, &["id"])?;
        if !seen_runs.insert(run_id) {
            bail!("workflow run {run_id} was observed more than once");
        }
        let run_source_sha = required_sha(&run, &["head_sha"])?;
        if run_source_sha != source_sha {
            bail!("workflow run {run_id} head SHA differs from requested source {source_sha}");
        }
        let latest_attempt = required_u32(&run, &["run_attempt"])?;
        validate_workflow_run(
            &run,
            &manifest.repository,
            repository_id,
            run_id,
            &run_source_sha,
            None,
        )?;
        let workflow_path = workflow_path_from_run(&run)?;
        let workflow_index = workflows
            .iter()
            .position(|workflow| {
                workflow.path == workflow_path && workflow.source_sha == run_source_sha
            })
            .unwrap_or(usize::MAX);
        let workflow_index = if workflow_index == usize::MAX {
            let workflow = collect_workflow_at_source(
                transport,
                store,
                auth,
                manifest,
                &workflow_path,
                &run_source_sha,
                ledger,
            )
            .await
            .with_context(|| {
                format!("collect workflow {workflow_path} at run {run_id} source {run_source_sha}")
            })?;
            workflows.push(workflow);
            workflows.len() - 1
        } else {
            workflow_index
        };
        let workflow = &workflows[workflow_index];
        let attempts = collect_attempt_chain(
            transport,
            store,
            auth,
            &manifest.repository,
            run_id,
            latest_attempt,
            collection_prefix,
            ledger,
        )
        .await?;
        let (artifact_values, artifact_raw_ids) = collect_items(
            transport,
            store,
            auth,
            ledger,
            github_workflow_artifacts_request(
                collection_id(
                    &manifest.repository,
                    &format!("{collection_prefix}-run-{run_id}-artifacts"),
                ),
                &manifest.repository,
                run_id,
            ),
        )
        .await?;
        let mut run_artifacts = artifact_values
            .iter()
            .map(|artifact| {
                parse_artifact(
                    artifact,
                    &manifest.repository,
                    repository_id,
                    run_id,
                    &run_source_sha,
                    artifact_raw_ids.clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut archive_raw_ids = Vec::new();
        for artifact in &mut run_artifacts {
            if artifact.expired != Some(false) {
                bail!(
                    "artifact {} in run {} is expired or has unknown expiration state",
                    artifact.artifact_id,
                    artifact.run_id
                );
            }
            let archive_result = collect_binary(
                transport,
                store,
                auth,
                super::RestCollectionRequest::new(
                    collection_id(
                        &manifest.repository,
                        &format!(
                            "{collection_prefix}-run-{}-artifact-{}-zip",
                            artifact.run_id, artifact.artifact_id
                        ),
                    ),
                    format!(
                        "/repos/{}/actions/artifacts/{}/zip",
                        manifest.repository, artifact.artifact_id
                    ),
                    None::<String>,
                    "workflow_artifacts",
                )
                .with_per_page(1),
            )
            .await
            .map_err(|error| anyhow!(acquisition_error_message(error)))?;
            if !archive_result.complete {
                bail!(
                    "artifact {} archive acquisition incomplete: {:?}",
                    artifact.artifact_id,
                    archive_result.state
                );
            }
            let archive_refs = archive_result
                .raw_objects
                .iter()
                .filter(|raw| {
                    raw.object_kind == "workflow_artifacts" && raw.sha256 == artifact.digest
                })
                .map(|raw| raw.raw_id.clone())
                .collect::<Vec<_>>();
            if archive_refs.len() != 1 {
                bail!(
                    "artifact {} archive bytes do not match metadata digest {}",
                    artifact.artifact_id,
                    artifact.digest
                );
            }
            archive_raw_ids.extend(archive_refs.iter().cloned());
            ledger.ingest(archive_result)?;
            // Keep the metadata-list response and the verified archive
            // response. Replacing metadata refs with archive refs loses the
            // provider object that established name/run/expiry.
            artifact.raw_object_refs.extend(archive_refs);
            artifact.raw_object_refs.sort();
            artifact.raw_object_refs.dedup();
        }
        artifacts.extend(run_artifacts);
        for (attempt, attempt_raw_ids, attempt_number) in attempts {
            let attempt_run_id = required_u64(&attempt, &["id"])?;
            if attempt_run_id != run_id {
                bail!("workflow attempt {attempt_number} belongs to run {attempt_run_id}, expected {run_id}");
            }
            let attempt_source_sha = required_sha(&attempt, &["head_sha"])?;
            if attempt_source_sha != run_source_sha {
                bail!("workflow attempt {run_id}/{attempt_number} head SHA differs from run");
            }
            validate_workflow_run(
                &attempt,
                &manifest.repository,
                repository_id,
                run_id,
                &run_source_sha,
                Some(attempt_number),
            )?;
            if workflow_path_from_run(&attempt)? != workflow_path {
                bail!("workflow attempt {run_id}/{attempt_number} path differs from run");
            }
            let event = required_string(&attempt, &["event"])?;
            let source_url = required_string(&attempt, &["html_url"])?;
            validate_workflow_attempt_url(
                &source_url,
                &manifest.repository,
                run_id,
                attempt_number,
            )?;
            let (jobs, jobs_raw_ids) = collect_items(
                transport,
                store,
                auth,
                ledger,
                github_workflow_attempt_jobs_request(
                    collection_id(
                        &manifest.repository,
                        &format!("run-{run_id}-attempt-{attempt_number}-jobs"),
                    ),
                    &manifest.repository,
                    run_id,
                    attempt_number as u64,
                ),
            )
            .await?;
            let live_jobs = jobs
                .iter()
                .map(|job| {
                    parse_job(
                        job,
                        &event,
                        &manifest.repository,
                        repository_id,
                        run_id,
                        attempt_number,
                        &run_source_sha,
                        jobs_raw_ids.clone(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            validate_unique_jobs(&live_jobs, run_id, attempt_number)?;
            let mut raw_ids = run_list_raw_ids.clone();
            raw_ids.extend(attempt_raw_ids);
            raw_ids.extend(jobs_raw_ids);
            raw_ids.extend(artifact_raw_ids.clone());
            raw_ids.extend(archive_raw_ids.clone());
            raw_ids.sort();
            raw_ids.dedup();
            executions.push(LiveExecution {
                run_id,
                run_attempt: attempt_number,
                workflow_path: workflow_path.clone(),
                workflow_revision: workflow.revision.clone(),
                event,
                source_sha: run_source_sha.clone(),
                checkout: LiveCheckoutObservation::api_head_only(
                    run_source_sha.clone(),
                    raw_ids.clone(),
                ),
                status: required_string(&attempt, &["status"])?,
                conclusion: optional_string(&attempt, &["conclusion"]),
                source_url,
                jobs: live_jobs,
                raw_object_refs: raw_ids,
            });
        }
    }
    Ok((executions, artifacts))
}

#[allow(
    clippy::too_many_arguments,
    reason = "attempt traversal keeps transport, storage, auth, run identity, phase, and ledger explicit"
)]
async fn collect_attempt_chain<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    repository: &str,
    run_id: u64,
    latest_attempt: u32,
    collection_prefix: &str,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<(Value, Vec<String>, u32)>>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    if latest_attempt == 0 {
        bail!("workflow run {run_id} has invalid run_attempt 0");
    }
    let mut next_attempt = latest_attempt;
    let mut seen_numbers = BTreeSet::new();
    let mut seen_identities = BTreeSet::new();
    let mut chain = Vec::new();
    loop {
        let (attempt, raw_ids) = collect_one(
            transport,
            store,
            auth,
            ledger,
            github_workflow_attempt_request(
                collection_id(
                    repository,
                    &format!("{collection_prefix}-run-{run_id}-attempt-{next_attempt}"),
                ),
                repository,
                run_id,
                next_attempt as u64,
            ),
        )
        .await?;
        let attempt_number = required_u32(&attempt, &["run_attempt"])?;
        if attempt_number != next_attempt {
            bail!(
                "workflow run {run_id} attempt endpoint returned attempt {attempt_number}, expected {next_attempt}"
            );
        }
        if !seen_numbers.insert(attempt_number) {
            bail!("workflow run {run_id} repeated attempt number {attempt_number}");
        }
        let attempt_id = required_u64(&attempt, &["id"])?;
        if attempt_id != run_id {
            bail!("workflow attempt {attempt_number} returned run {attempt_id}, expected {run_id}");
        }
        if !seen_identities.insert((attempt_id, attempt_number)) {
            bail!("workflow run {run_id} repeated attempt identity {attempt_id}/{attempt_number}");
        }
        let previous_url = optional_string(&attempt, &["previous_attempt_url"]);
        chain.push((attempt, raw_ids, attempt_number));
        let Some(previous_url) = previous_url else {
            if next_attempt > 1 {
                bail!("workflow run {run_id} attempt chain omitted previous attempt URL");
            }
            break;
        };
        let previous_attempt = parse_previous_attempt_url(&previous_url, repository, run_id)?;
        if previous_attempt + 1 != next_attempt {
            bail!(
                "workflow run {run_id} attempt chain jumps from {next_attempt} to {previous_attempt}"
            );
        }
        next_attempt = previous_attempt;
    }
    Ok(chain)
}

fn parse_previous_attempt_url(url: &str, repository: &str, run_id: u64) -> Result<u32> {
    let parsed = Url::parse(url).context("parse previous workflow attempt URL")?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("api.github.com")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        bail!("previous workflow attempt URL has an unsafe origin or components");
    }
    let expected_prefix = format!("/repos/{repository}/actions/runs/{run_id}/attempts/");
    let path = parsed.path();
    let suffix = path
        .strip_prefix(&expected_prefix)
        .ok_or_else(|| anyhow!("previous workflow attempt URL is not bound to run {run_id}"))?;
    let attempt = suffix
        .parse::<u32>()
        .context("previous workflow attempt URL has invalid attempt number")?;
    if attempt == 0 {
        bail!("previous workflow attempt URL has attempt number 0");
    }
    Ok(attempt)
}

async fn collect_check_facts<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    source_sha: &str,
    collection_prefix: &str,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<LiveCheck>>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let (suites, suite_raw_ids) = collect_items(
        transport,
        store,
        auth,
        ledger,
        github_check_suites_request(
            collection_id(
                &manifest.repository,
                &format!("{collection_prefix}-check-suites-{source_sha}"),
            ),
            &manifest.repository,
            source_sha,
        ),
    )
    .await?;
    let mut checks = Vec::new();
    let mut seen_suite_ids = BTreeSet::new();
    for suite in suites {
        let suite_id = required_u64(&suite, &["id"])?;
        if !seen_suite_ids.insert(suite_id) {
            bail!("check suite {suite_id} was observed more than once");
        }
        validate_check_suite(&suite, &manifest.repository, source_sha, suite_id)?;
        let (runs, run_raw_ids) = collect_items(
            transport,
            store,
            auth,
            ledger,
            github_check_suite_runs_request(
                collection_id(
                    &manifest.repository,
                    &format!("{collection_prefix}-suite-{suite_id}-runs"),
                ),
                &manifest.repository,
                suite_id,
            ),
        )
        .await?;
        for run in runs {
            let mut raw_ids = suite_raw_ids.clone();
            raw_ids.extend(run_raw_ids.clone());
            checks.push(parse_check(
                &run,
                &manifest.repository,
                source_sha,
                raw_ids,
                suite_id,
            )?);
        }
    }
    checks.sort_by_key(|check| check.check_run_id);
    for pair in checks.windows(2) {
        if pair[0].check_run_id == pair[1].check_run_id {
            bail!(
                "check run {} was observed more than once",
                pair[0].check_run_id
            );
        }
    }
    Ok(checks)
}

fn validate_unique_jobs(jobs: &[LiveJob], run_id: u64, run_attempt: u32) -> Result<()> {
    let mut job_ids = BTreeSet::new();
    let mut check_run_ids = BTreeSet::new();
    for job in jobs {
        if !job_ids.insert(job.job_id) {
            bail!(
                "workflow run {run_id} attempt {run_attempt} repeated job {}",
                job.job_id
            );
        }
        if !check_run_ids.insert(job.check_run_id) {
            bail!(
                "workflow run {run_id} attempt {run_attempt} repeated check run {}",
                job.check_run_id
            );
        }
    }
    Ok(())
}

fn workflow_bindings(executions: &[LiveExecution]) -> Result<Vec<LiveWorkflowBinding>> {
    let mut grouped = BTreeMap::<(String, String, String, String), LiveWorkflowBinding>::new();
    for execution in executions {
        let key = (
            execution.workflow_path.clone(),
            execution.workflow_revision.clone(),
            execution.event.clone(),
            execution.source_sha.clone(),
        );
        let entry = grouped.entry(key).or_insert_with(|| LiveWorkflowBinding {
            workflow_path: execution.workflow_path.clone(),
            workflow_revision: execution.workflow_revision.clone(),
            event: execution.event.clone(),
            source_sha: execution.source_sha.clone(),
            checkout: execution.checkout.clone(),
            run_ids: Vec::new(),
            raw_object_refs: Vec::new(),
        });
        entry.run_ids.push(execution.run_id);
        match (&entry.checkout.proof, &execution.checkout.proof) {
            (Some(existing), Some(incoming)) if existing != incoming => {
                bail!(
                    "workflow {} event {} has conflicting checkout proofs",
                    entry.workflow_path,
                    entry.event
                );
            }
            (None, Some(_)) => {
                entry.checkout = execution.checkout.clone();
            }
            _ => {}
        }
        entry
            .raw_object_refs
            .extend(execution.raw_object_refs.clone());
    }
    Ok(grouped
        .into_values()
        .map(|mut binding| {
            binding.run_ids.sort();
            binding.run_ids.dedup();
            binding.raw_object_refs.sort();
            binding.raw_object_refs.dedup();
            binding
        })
        .collect())
}

fn parse_pull_request_identity(
    value: &Value,
    raw_object_refs: Vec<String>,
) -> Result<LivePullRequestIdentity> {
    let head_sha = required_sha(value, &["head", "sha"])?;
    let base_sha = required_sha(value, &["base", "sha"])?;
    let tested_merge_sha = optional_revision(value, &["merge_commit_sha"])?;
    let merge_group_sha = optional_revision(value, &["merge_group", "sha"])?;
    Ok(LivePullRequestIdentity {
        number: required_u64(value, &["number"])?,
        state: required_string(value, &["state"])?,
        draft: value
            .get("draft")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow!("PR draft state missing"))?,
        author: required_string(value, &["user", "login"])?,
        author_association: required_string(value, &["author_association"])?,
        head_repository: optional_string(value, &["head", "repo", "full_name"]),
        head_ref: optional_string(value, &["head", "ref"]),
        head_sha,
        base_repository: optional_string(value, &["base", "repo", "full_name"]),
        base_ref: required_string(value, &["base", "ref"])?,
        base_sha,
        tested_merge_sha,
        merge_group_sha,
        source_url: required_string(value, &["html_url"])?,
        raw_object_refs,
    })
}

fn parse_ruleset_checks(
    value: &Value,
    ruleset_id: u64,
    raw_ids: Vec<String>,
) -> Result<Vec<LiveRulesetCheck>> {
    let Some(rules) = value.get("rules").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut checks = Vec::new();
    for rule in rules {
        let Some(parameters) = rule.get("parameters") else {
            continue;
        };
        let Some(required) = parameters
            .get("required_status_checks")
            .or_else(|| parameters.get("required_status_checks_and_apps"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for check in required {
            let context = check
                .get("context")
                .or_else(|| check.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("ruleset {ruleset_id} check lacks context"))?;
            let app_id = check
                .get("integration_id")
                .or_else(|| check.get("app_id"))
                .and_then(|value| value.as_u64().map(|id| id.to_string()));
            checks.push(LiveRulesetCheck {
                context: context.to_owned(),
                app_id,
                ruleset_id,
                raw_object_refs: raw_ids.clone(),
            });
        }
    }
    checks.sort_by(|left, right| {
        left.context
            .cmp(&right.context)
            .then(left.app_id.cmp(&right.app_id))
    });
    for pair in checks.windows(2) {
        if pair[0].context == pair[1].context && pair[0].app_id == pair[1].app_id {
            bail!(
                "ruleset {ruleset_id} repeats required check {}",
                pair[0].context
            );
        }
    }
    Ok(checks)
}

fn ruleset_is_complete(value: &Value) -> bool {
    ["id", "name", "target", "enforcement", "rules"]
        .into_iter()
        .all(|field| value.get(field).is_some())
        && value.get("rules").and_then(Value::as_array).is_some()
}

#[allow(
    clippy::too_many_arguments,
    reason = "job parsing keeps repository, run, attempt, source, and raw provenance explicit"
)]
fn parse_job(
    value: &Value,
    event: &str,
    repository: &str,
    repository_id: u64,
    run_id: u64,
    run_attempt: u32,
    source_sha: &str,
    raw_object_refs: Vec<String>,
) -> Result<LiveJob> {
    let job_run_id = required_u64(value, &["run_id"])?;
    if job_run_id != run_id {
        bail!("job belongs to run {job_run_id}, expected {run_id}");
    }
    let job_attempt = required_u32(value, &["run_attempt"])?;
    if job_attempt != run_attempt {
        bail!("job belongs to attempt {job_attempt}, expected {run_attempt}");
    }
    let job_source_sha = required_sha(value, &["head_sha"])?;
    if job_source_sha != source_sha {
        bail!("job head SHA differs from workflow run");
    }
    let job_id = required_u64(value, &["id"])?;
    let source_url = required_string(value, &["html_url"])?;
    validate_job_url(&source_url, repository, run_id, job_id)?;
    validate_api_url(
        &required_string(value, &["url"])?,
        &format!("/repos/{repository}/actions/jobs/{job_id}"),
        "job API URL",
    )?;
    validate_api_url(
        &required_string(value, &["run_url"])?,
        &format!("/repos/{repository}/actions/runs/{run_id}"),
        "job run URL",
    )?;
    let job_repository_id = optional_u64(value, &["repository", "id"]);
    if job_repository_id.is_some_and(|id| id != repository_id) {
        bail!(
            "job belongs to repository ID {:?}, expected {repository_id}",
            job_repository_id
        );
    }
    let check_run_url = required_string(value, &["check_run_url"])?;
    let check_run_id = parse_check_run_api_url(&check_run_url, repository)?;
    Ok(LiveJob {
        job_id,
        check_run_id,
        run_id: job_run_id,
        run_attempt: job_attempt,
        name: required_string(value, &["name"])?,
        status: required_string(value, &["status"])?,
        conclusion: optional_string(value, &["conclusion"]),
        event: event.to_owned(),
        source_sha: Some(job_source_sha.clone()),
        checkout: LiveCheckoutObservation::api_head_only(job_source_sha, raw_object_refs.clone()),
        source_url,
        raw_object_refs,
    })
}

fn parse_artifact(
    value: &Value,
    repository: &str,
    repository_id: u64,
    run_id: u64,
    run_head_sha: &str,
    raw_object_refs: Vec<String>,
) -> Result<LiveArtifact> {
    let artifact_run_id = required_u64(value, &["workflow_run", "id"])?;
    if artifact_run_id != run_id {
        bail!("artifact belongs to run {artifact_run_id}, expected {run_id}");
    }
    let artifact_head_sha = required_sha(value, &["workflow_run", "head_sha"])?;
    if artifact_head_sha != run_head_sha {
        bail!("artifact workflow head SHA differs from run");
    }
    let artifact_repository_id = required_u64(value, &["workflow_run", "repository_id"])?;
    if artifact_repository_id != repository_id {
        bail!(
            "artifact belongs to repository ID {artifact_repository_id}, expected {repository_id}"
        );
    }
    let artifact_id = required_u64(value, &["id"])?;
    validate_api_url(
        &required_string(value, &["url"])?,
        &format!("/repos/{repository}/actions/artifacts/{artifact_id}"),
        "artifact API URL",
    )?;
    let source_url = required_string(value, &["archive_download_url"])?;
    validate_artifact_url(&source_url, repository, artifact_id)?;
    let digest = required_string(value, &["digest"])?;
    if !is_digest(&digest) {
        bail!("artifact {} has malformed digest", artifact_id);
    }
    Ok(LiveArtifact {
        artifact_id,
        run_id: artifact_run_id,
        run_attempt: None,
        run_head_sha: artifact_head_sha,
        name: required_string(value, &["name"])?,
        digest,
        expired: value.get("expired").and_then(Value::as_bool),
        source_url,
        raw_object_refs,
    })
}

fn merge_artifacts(destination: &mut Vec<LiveArtifact>, incoming: Vec<LiveArtifact>) -> Result<()> {
    for artifact in incoming {
        if destination.iter().any(|existing| {
            existing.run_id == artifact.run_id
                && existing.name == artifact.name
                && existing.artifact_id != artifact.artifact_id
        }) {
            bail!(
                "artifact name {} is bound to more than one artifact ID",
                artifact.name
            );
        }
        if destination
            .iter()
            .any(|existing| existing.artifact_id == artifact.artifact_id)
        {
            bail!(
                "artifact {} was observed more than once",
                artifact.artifact_id
            );
        }
        destination.push(artifact);
    }
    destination.sort_by_key(|artifact| artifact.artifact_id);
    Ok(())
}

fn access_gaps(repository: &Value) -> Vec<String> {
    let mut gaps = Vec::new();
    let Some(permissions) = repository.get("permissions").and_then(Value::as_object) else {
        gaps.push("repository.permissions".to_owned());
        return gaps;
    };
    if permissions.get("pull").and_then(Value::as_bool) != Some(true) {
        gaps.push("repository.permissions.pull".to_owned());
    }
    if repository.get("private").and_then(Value::as_bool).is_none() {
        gaps.push("repository.private".to_owned());
    }
    if repository
        .get("visibility")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        gaps.push("repository.visibility".to_owned());
    }
    if repository
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        gaps.push("repository.owner.login".to_owned());
    }
    gaps
}

fn access_endpoint_gaps(requests: &[RequestRecord], repository: &str) -> Vec<String> {
    let prefix = format!("/repos/{repository}");
    let mut observed = BTreeSet::new();
    for request in requests {
        if !request.complete
            || !matches!(
                request.state,
                AcquisitionState::Complete | AcquisitionState::EmptyComplete
            )
            || request.response_raw_ref.is_none()
        {
            continue;
        }
        let endpoint = request.endpoint_or_operation.as_str();
        if endpoint == prefix {
            observed.insert("repository".to_owned());
        } else if endpoint == format!("{prefix}/rulesets") {
            observed.insert("rulesets".to_owned());
        } else if endpoint == format!("{prefix}/actions/workflows") {
            observed.insert("workflow_inventory".to_owned());
        } else if endpoint.starts_with(&format!("{prefix}/contents/")) {
            observed.insert("workflow_source".to_owned());
        } else if endpoint == format!("{prefix}/pulls") {
            observed.insert("pull_requests".to_owned());
        } else if endpoint == format!("{prefix}/actions/runs") {
            observed.insert("workflow_runs".to_owned());
        } else if endpoint.contains("/actions/runs/") && endpoint.ends_with("/jobs") {
            observed.insert("workflow_jobs".to_owned());
        } else if endpoint.contains("/actions/runs/") && endpoint.contains("/attempts/") {
            observed.insert("workflow_attempts".to_owned());
        } else if endpoint.contains("/actions/runs/") && endpoint.ends_with("/artifacts") {
            observed.insert("workflow_artifacts".to_owned());
        } else if endpoint.contains("/commits/") && endpoint.ends_with("/check-suites") {
            observed.insert("check_suites".to_owned());
        } else if endpoint.contains("/check-suites/") && endpoint.ends_with("/check-runs") {
            observed.insert("check_runs".to_owned());
        } else if endpoint.starts_with(&format!("{prefix}/commits/")) {
            observed.insert("default_commit".to_owned());
        }
    }
    [
        "repository",
        "default_commit",
        "rulesets",
        "workflow_inventory",
        "workflow_source",
        "pull_requests",
        "workflow_runs",
        "workflow_attempts",
        "workflow_jobs",
        "workflow_artifacts",
        "check_suites",
        "check_runs",
    ]
    .into_iter()
    .filter(|capability| !observed.contains(*capability))
    .map(|capability| format!("api.{capability}"))
    .collect()
}

fn validate_workflow_attempt_url(
    value: &str,
    repository: &str,
    run_id: u64,
    attempt: u32,
) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    let expected = format!("/{repository}/actions/runs/{run_id}/attempts/{attempt}");
    if parsed.path() != expected {
        bail!("workflow attempt URL is not bound to {repository}/{run_id}/{attempt}");
    }
    Ok(())
}

fn validate_workflow_run(
    value: &Value,
    repository: &str,
    repository_id: u64,
    run_id: u64,
    source_sha: &str,
    attempt: Option<u32>,
) -> Result<()> {
    if required_u64(value, &["id"])? != run_id || required_sha(value, &["head_sha"])? != source_sha
    {
        bail!("workflow run identity does not match requested run/source");
    }
    let actual_repository_id = required_u64(value, &["repository", "id"])?;
    let actual_repository = required_string(value, &["repository", "full_name"])?;
    if actual_repository_id != repository_id || actual_repository != repository {
        bail!(
            "workflow run belongs to {actual_repository} ({actual_repository_id}), expected {repository} ({repository_id})"
        );
    }
    let api_path = match attempt {
        Some(attempt) => format!("/repos/{repository}/actions/runs/{run_id}/attempts/{attempt}"),
        None => format!("/repos/{repository}/actions/runs/{run_id}"),
    };
    validate_api_url(
        &required_string(value, &["url"])?,
        &api_path,
        "workflow run API URL",
    )?;
    let html_path = match attempt {
        Some(attempt) => format!("/{repository}/actions/runs/{run_id}/attempts/{attempt}"),
        None => format!("/{repository}/actions/runs/{run_id}"),
    };
    validate_github_url(
        &required_string(value, &["html_url"])?,
        &html_path,
        "workflow run HTML URL",
    )?;
    Ok(())
}

fn workflow_path_from_run(value: &Value) -> Result<String> {
    let raw = required_string(value, &["path"])?;
    let path =
        raw.rsplit_once('@').map_or(
            raw.as_str(),
            |(path, reference)| {
                if reference.is_empty() {
                    ""
                } else {
                    path
                }
            },
        );
    if path.trim().is_empty() {
        bail!("workflow run path is empty");
    }
    Ok(path.to_owned())
}

fn validate_repository_url(value: &str, repository: &str) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    let expected = format!("/{repository}");
    if parsed.path() != expected {
        bail!("repository URL is not bound to {repository}");
    }
    Ok(())
}

fn validate_pull_request_identity(
    identity: &LivePullRequestIdentity,
    repository: &str,
) -> Result<()> {
    let parsed = parse_safe_url(&identity.source_url, "github.com")?;
    let expected = format!("/{repository}/pull/{}", identity.number);
    if parsed.path() != expected {
        bail!(
            "pull request URL is not bound to {repository}#{}",
            identity.number
        );
    }
    if identity.base_repository.as_deref() != Some(repository) {
        bail!(
            "pull request #{} base repository is not {repository}",
            identity.number
        );
    }
    if let Some(head_repository) = &identity.head_repository {
        validate_repository_slug(head_repository)?;
    }
    Ok(())
}

fn validate_workflow_content(
    value: &Value,
    repository: &str,
    path: &str,
    source_sha: &str,
) -> Result<()> {
    if required_string(value, &["path"])? != path
        || !is_hex_revision(&required_string(value, &["sha"])?, 40)
    {
        bail!("workflow content identity does not match requested path/revision");
    }
    if let Some(content_repository) = optional_string(value, &["repository", "full_name"])
        && content_repository != repository
    {
        bail!("workflow content belongs to {content_repository}, expected {repository}");
    }
    let content_url = required_string(value, &["url"])?;
    let parsed = Url::parse(&content_url).context("parse workflow content URL")?;
    let mut query = parsed.query_pairs();
    let has_exact_ref = query
        .next()
        .is_some_and(|(key, value)| key == "ref" && value == source_sha)
        && query.next().is_none();
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("api.github.com")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != format!("/repos/{repository}/contents/{}", encode_api_path(path))
        || !has_exact_ref
    {
        bail!("workflow content URL is not bound to requested repository/path/ref");
    }
    Ok(())
}

fn encode_api_path(value: &str) -> String {
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

fn validate_job_url(value: &str, repository: &str, run_id: u64, job_id: u64) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    let documented = format!("/{repository}/runs/{run_id}/jobs/{job_id}");
    if parsed.path() != documented {
        bail!("job URL is not bound to {repository}/{run_id}/{job_id}");
    }
    Ok(())
}

fn validate_api_url(value: &str, expected_path: &str, label: &str) -> Result<()> {
    let parsed = parse_safe_url(value, "api.github.com")?;
    if parsed.path() != expected_path {
        bail!("{label} is not bound to {expected_path}");
    }
    Ok(())
}

fn validate_github_url(value: &str, expected_path: &str, label: &str) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    if parsed.path() != expected_path {
        bail!("{label} is not bound to {expected_path}");
    }
    Ok(())
}

fn validate_artifact_url(value: &str, repository: &str, artifact_id: u64) -> Result<()> {
    let parsed = parse_safe_url(value, "api.github.com")?;
    let expected = format!("/repos/{repository}/actions/artifacts/{artifact_id}/zip");
    if parsed.path() != expected {
        bail!("artifact URL is not bound to {repository}/{artifact_id}");
    }
    Ok(())
}

fn parse_safe_url(value: &str, host: &str) -> Result<Url> {
    let parsed = Url::parse(value).context("parse GitHub URL")?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(host)
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("GitHub URL has an unsafe origin or components");
    }
    Ok(parsed)
}

fn validate_check_run_url(value: &str, repository: &str, check_run_id: u64) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    let expected = format!("/{repository}/runs/{check_run_id}");
    if parsed.path() != expected {
        bail!("check run URL is not bound to {repository}/{check_run_id}");
    }
    Ok(())
}

fn parse_check_run_api_url(value: &str, repository: &str) -> Result<u64> {
    let parsed = parse_safe_url(value, "api.github.com")?;
    let prefix = format!("/repos/{repository}/check-runs/");
    let suffix = parsed
        .path()
        .strip_prefix(&prefix)
        .ok_or_else(|| anyhow!("job check-run URL is not bound to {repository}"))?;
    let check_run_id = suffix
        .parse::<u64>()
        .context("job check-run URL has invalid check-run ID")?;
    if check_run_id == 0 {
        bail!("job check-run URL has check-run ID 0");
    }
    Ok(check_run_id)
}

fn validate_check_suite(
    value: &Value,
    repository: &str,
    source_sha: &str,
    suite_id: u64,
) -> Result<()> {
    if required_u64(value, &["id"])? != suite_id
        || required_sha(value, &["head_sha"])? != source_sha
    {
        bail!("check suite identity does not match requested source");
    }
    let suite_repository = value
        .get("repository")
        .and_then(|repository| repository.get("full_name"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("check suite lacks repository identity"))?;
    if suite_repository != repository {
        bail!("check suite belongs to {suite_repository}, expected {repository}");
    }
    let source_url = required_string(value, &["url"])?;
    let parsed = parse_safe_url(&source_url, "api.github.com")?;
    let expected = format!("/repos/{repository}/check-suites/{suite_id}");
    if parsed.path() != expected {
        bail!("check suite URL is not bound to {repository}/{suite_id}");
    }
    let check_runs_url = required_string(value, &["check_runs_url"])?;
    let parsed = parse_safe_url(&check_runs_url, "api.github.com")?;
    let expected = format!("{expected}/check-runs");
    if parsed.path() != expected {
        bail!("check suite check-runs URL is not bound to {repository}/{suite_id}");
    }
    Ok(())
}

fn parse_check(
    value: &Value,
    repository: &str,
    source_sha: &str,
    raw_ids: Vec<String>,
    suite_id: u64,
) -> Result<LiveCheck> {
    let check_suite_id = required_u64(value, &["check_suite", "id"])?;
    if check_suite_id != suite_id {
        bail!("check run belongs to suite {check_suite_id}, expected {suite_id}");
    }
    let check_source_sha = required_sha(value, &["head_sha"])?;
    if check_source_sha != source_sha {
        bail!("check run head SHA differs from requested source");
    }
    let check_repository = optional_string(value, &["repository", "full_name"])
        .or_else(|| optional_string(value, &["check_suite", "repository", "full_name"]))
        .ok_or_else(|| anyhow!("check run lacks repository identity"))?;
    if check_repository != repository {
        bail!("check run belongs to {check_repository}, expected {repository}");
    }
    let workflow_run_id = optional_u64(value, &["check_suite", "workflow_run", "id"]);
    if workflow_run_id.is_some()
        && optional_revision(value, &["check_suite", "workflow_run", "head_sha"])?.as_deref()
            != Some(source_sha)
    {
        bail!("associated workflow run head SHA differs from check run");
    }
    let app_id = optional_u64(value, &["app", "id"]).map(|id| id.to_string());
    let app_slug = required_string(value, &["app", "slug"])?;
    let check_run_id = required_u64(value, &["id"])?;
    validate_api_url(
        &required_string(value, &["url"])?,
        &format!("/repos/{repository}/check-runs/{check_run_id}"),
        "check run API URL",
    )?;
    let source_url = required_string(value, &["html_url"])?;
    validate_check_run_url(&source_url, repository, check_run_id)?;
    Ok(LiveCheck {
        context: required_string(value, &["name"])?,
        app_id,
        app_slug,
        check_suite_id: Some(check_suite_id),
        check_run_id,
        workflow_run_id,
        job_id: None,
        run_attempt: optional_u32(value, &["check_suite", "workflow_run", "run_attempt"])?,
        source_sha: source_sha.to_owned(),
        checkout: LiveCheckoutObservation::api_head_only(source_sha.to_owned(), raw_ids.clone()),
        event: optional_string(value, &["check_suite", "workflow_run", "event"]),
        status: required_string(value, &["status"])?,
        conclusion: optional_string(value, &["conclusion"]),
        source_url,
        raw_object_refs: raw_ids,
    })
}

fn decode_workflow_content(value: &Value) -> Result<String> {
    let content = required_string(value, &["content"])?;
    let encoded = content.replace(['\n', '\r'], "");
    let bytes = BASE64
        .decode(encoded)
        .context("decode workflow content from GitHub")?;
    String::from_utf8(bytes).context("workflow content is not UTF-8")
}

fn parse_workflow_events(source: &str) -> Result<Vec<String>> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(source).context("parse workflow YAML")?;
    let mapping = yaml
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow YAML root is not a mapping"))?;
    let on = mapping
        .get("on")
        .or_else(|| mapping.get("true"))
        .ok_or_else(|| anyhow!("workflow lacks on trigger declaration"))?;
    let mut events = match on {
        serde_yaml::Value::Mapping(map) => map.keys().cloned().collect::<Vec<_>>(),
        serde_yaml::Value::Sequence(sequence) => sequence
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("workflow trigger list contains non-string"))?,
        serde_yaml::Value::String(value) => vec![value.clone()],
        serde_yaml::Value::Null => vec!["on".to_owned()],
        _ => bail!("workflow trigger declaration has invalid shape"),
    };
    events.sort();
    events.dedup();
    if events.is_empty() {
        bail!("workflow trigger declaration is empty")
    }
    Ok(events)
}

fn parse_source_jobs(
    source: &str,
    manifest: &ManifestRepository,
    raw_object_refs: &[String],
) -> Result<Vec<LiveSourceJob>> {
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(source).context("parse workflow source jobs")?;
    let jobs = yaml
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| anyhow!("workflow lacks jobs mapping"))?;
    let mut job_ids = jobs
        .keys()
        .map(|key| {
            let value = key.as_str();
            if value.trim().is_empty() {
                Err(anyhow!("workflow job ID is not a non-empty string"))
            } else {
                Ok(value.to_owned())
            }
        })
        .collect::<Result<Vec<_>>>()?;
    job_ids.sort();
    job_ids.dedup();
    if job_ids.is_empty() {
        bail!("workflow source has no jobs");
    }
    let mut expected = manifest
        .expected_jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    expected.sort();
    expected.dedup();
    if job_ids != expected {
        bail!(
            "workflow source job IDs do not match reviewed expected jobs for {}",
            manifest.repository
        );
    }
    Ok(job_ids
        .into_iter()
        .map(|job_id| LiveSourceJob {
            job_id,
            raw_object_refs: raw_object_refs.to_vec(),
        })
        .collect())
}

fn parse_workflow_dependencies(
    source: &str,
    current_repository: &str,
    source_sha: &str,
    raw_ids: &[String],
) -> Result<(
    Vec<LiveDependency>,
    Vec<LiveDependency>,
    Vec<LiveDependency>,
)> {
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(source).context("parse workflow dependencies")?;
    let mut uses = Vec::new();
    collect_uses(&yaml, &mut uses);
    let mut reusable = Vec::new();
    let mut actions = Vec::new();
    let mut scanners = Vec::new();
    for use_value in uses {
        let dependency = parse_dependency(&use_value, current_repository, source_sha, raw_ids)?;
        if dependency.kind == "reusable_workflow" {
            reusable.push(dependency);
        } else if dependency.kind == "scanner" {
            scanners.push(dependency);
        } else {
            actions.push(dependency);
        }
    }
    Ok((reusable, actions, scanners))
}

struct WorkflowDependencyContext<'a, 'ledger, 'progress, T: ?Sized, S> {
    transport: &'a T,
    store: &'a mut S,
    auth: &'a AuthIdentity,
    manifest: &'a ManifestRepository,
    workflow_path: &'a str,
    source_sha: &'a str,
    ledger: &'ledger mut Ledger<'progress>,
}

struct WorkflowDependencyGroups {
    reusable: Vec<LiveDependency>,
    actions: Vec<LiveDependency>,
    scanners: Vec<LiveDependency>,
}

async fn bind_workflow_dependencies<T, S>(
    context: WorkflowDependencyContext<'_, '_, '_, T, S>,
    groups: WorkflowDependencyGroups,
) -> Result<WorkflowDependencyGroups>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let WorkflowDependencyContext {
        transport,
        store,
        auth,
        manifest,
        workflow_path,
        source_sha,
        ledger,
    } = context;
    let mut dependencies = groups
        .reusable
        .into_iter()
        .chain(groups.actions)
        .chain(groups.scanners)
        .enumerate()
        .collect::<Vec<_>>();
    for (index, dependency) in &mut dependencies {
        if !is_hex_revision(&dependency.revision, 40) {
            bail!(
                "workflow dependency {}/{} remains unresolved at {}",
                dependency.repository,
                dependency.path,
                dependency.revision
            );
        }
        let tree_endpoint = format!(
            "/repos/{}/git/trees/{}?recursive=1",
            dependency.repository, dependency.revision
        );
        let tree_key = DependencyTreeKey {
            endpoint: tree_endpoint.clone(),
        };
        let tree_was_cached = ledger.dependency_trees.contains_key(&tree_key);
        let (tree, raw_ids) =
            if let Some((tree, raw_ids)) = ledger.dependency_trees.get(&tree_key).cloned() {
                (tree, raw_ids)
            } else {
                collect_one(
                    transport,
                    store,
                    auth,
                    ledger,
                    github_single_object_request(
                        collection_id(
                            &manifest.repository,
                            &format!(
                                "workflow-dependency-{}-{}-{}",
                                safe_id(workflow_path),
                                safe_id(source_sha),
                                index
                            ),
                        ),
                        tree_endpoint,
                        "workflow.dependency",
                    ),
                )
                .await
                .with_context(|| {
                    format!(
                        "workflow dependency tree {}/{}@{}",
                        dependency.repository, dependency.path, dependency.revision
                    )
                })?
            };
        let resolved_path =
            validate_dependency_tree(&tree, &dependency.repository, &dependency.path)?;
        if !tree_was_cached {
            ledger
                .dependency_trees
                .insert(tree_key, (tree.clone(), raw_ids.clone()));
        }
        let encoded_path = encode_api_path(&resolved_path);
        let source_endpoint = format!(
            "/repos/{}/contents/{encoded_path}?ref={}",
            dependency.repository, dependency.revision
        );
        let source_key = DependencySourceKey {
            metadata_endpoint: source_endpoint.clone(),
            raw_endpoint: source_endpoint.clone(),
        };
        let cached_source = if let Some(cached) = ledger.dependency_sources.get(&source_key) {
            cached.clone()
        } else {
            let (source, _source_metadata_raw_ids) = collect_one(
                transport,
                store,
                auth,
                ledger,
                github_single_object_request(
                    collection_id(
                        &manifest.repository,
                        &format!(
                            "workflow-dependency-source-{}-{}-{}",
                            safe_id(workflow_path),
                            safe_id(source_sha),
                            index
                        ),
                    ),
                    source_endpoint,
                    "workflow.dependency.source",
                ),
            )
            .await
            .with_context(|| {
                format!(
                    "workflow dependency source {}/{}@{}",
                    dependency.repository, resolved_path, dependency.revision
                )
            })?;
            validate_workflow_content(
                &source,
                &dependency.repository,
                &resolved_path,
                &dependency.revision,
            )?;
            let source_url = required_string(&source, &["url"])?;
            let metadata_source_text = decode_workflow_content(&source)?;
            let (source_text, source_raw_ids) = collect_raw_source(
                transport,
                store,
                auth,
                ledger,
                collection_id(
                    &manifest.repository,
                    &format!(
                        "workflow-dependency-raw-source-{}-{}-{}",
                        safe_id(workflow_path),
                        safe_id(source_sha),
                        index
                    ),
                ),
                &dependency.repository,
                &resolved_path,
                &dependency.revision,
                "workflow.dependency.source",
            )
            .await?;
            if source_text != metadata_source_text {
                bail!(
                    "workflow dependency source metadata and raw media bytes differ for {}/{}@{}",
                    dependency.repository,
                    resolved_path,
                    dependency.revision
                );
            }
            let cached = CachedDependencySource {
                source_url,
                source_text,
                source_raw_object_refs: source_raw_ids,
            };
            ledger.dependency_sources.insert(source_key, cached.clone());
            cached
        };
        dependency.resolved_path = Some(resolved_path);
        dependency.source_url = Some(cached_source.source_url);
        dependency.source_bytes_base64 = Some(BASE64.encode(cached_source.source_text.as_bytes()));
        dependency.source_raw_object_refs = cached_source.source_raw_object_refs;
        dependency.raw_object_refs = raw_ids;
    }
    let mut reusable = Vec::new();
    let mut actions = Vec::new();
    let mut scanners = Vec::new();
    for (_, dependency) in dependencies {
        if dependency.kind == "reusable_workflow" {
            reusable.push(dependency);
        } else if dependency.kind == "scanner" {
            scanners.push(dependency);
        } else {
            actions.push(dependency);
        }
    }
    Ok(WorkflowDependencyGroups {
        reusable,
        actions,
        scanners,
    })
}

fn validate_dependency_tree(value: &Value, repository: &str, path: &str) -> Result<String> {
    if value.get("truncated").and_then(Value::as_bool) != Some(false) {
        bail!("dependency tree {repository}@{path} is truncated");
    }
    let tree = value
        .get("tree")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("dependency tree {repository}@{path} lacks tree entries"))?;
    let requested = path
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/');
    let mut candidates = Vec::new();
    if requested.is_empty() || requested == "." {
        candidates.push("action.yml".to_owned());
        candidates.push("action.yaml".to_owned());
    } else if requested.ends_with(".yml") || requested.ends_with(".yaml") {
        candidates.push(requested.to_owned());
    } else {
        candidates.push(format!("{requested}/action.yml"));
        candidates.push(format!("{requested}/action.yaml"));
    }
    let found = tree.iter().find_map(|entry| {
        let entry_path = entry.get("path").and_then(Value::as_str)?;
        candidates
            .iter()
            .find(|candidate| {
                entry_path == candidate.as_str()
                    && entry.get("type").and_then(Value::as_str) == Some("blob")
            })
            .cloned()
    });
    let Some(found) = found else {
        bail!("dependency target {repository}/{requested} is absent from immutable tree");
    };
    Ok(found)
}

fn collect_uses(value: &serde_yaml::Value, output: &mut Vec<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (key, value) in map {
                if key.as_str() == "uses"
                    && let Some(value) = value.as_str()
                {
                    output.push(value.to_owned());
                }
                collect_uses(value, output);
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                collect_uses(value, output);
            }
        }
        _ => {}
    }
}

fn parse_dependency(
    uses: &str,
    current_repository: &str,
    local_revision: &str,
    _raw_ids: &[String],
) -> Result<LiveDependency> {
    let (target, revision) = if let Some((target, revision)) = uses.rsplit_once('@') {
        (target, revision.to_owned())
    } else if uses.starts_with("./") && !local_revision.trim().is_empty() {
        (uses, local_revision.to_owned())
    } else {
        bail!("workflow dependency {uses} lacks immutable revision");
    };
    if target.trim().is_empty() || revision.trim().is_empty() {
        bail!("workflow dependency {uses} has empty target or revision");
    }
    if !is_hex_revision(&revision, 40) {
        bail!("workflow dependency {uses} is not pinned to an immutable 40-hex revision");
    }
    let (repository, path) = if target.starts_with("./") {
        (current_repository.to_owned(), target.to_owned())
    } else {
        let mut parts = target.splitn(3, '/');
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if owner.is_empty() || name.is_empty() {
            bail!("workflow dependency {uses} lacks repository identity");
        }
        (
            format!("{owner}/{name}"),
            parts.next().unwrap_or(".").to_owned(),
        )
    };
    validate_repository_slug(&repository)?;
    let lower = uses.to_ascii_lowercase();
    let kind = if path.ends_with(".yml") || path.ends_with(".yaml") || path.contains("/workflows/")
    {
        "reusable_workflow"
    } else if ["codeql", "sonar", "semgrep", "trivy", "snyk", "dco"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        "scanner"
    } else {
        "action"
    };
    Ok(LiveDependency {
        kind: kind.to_owned(),
        repository,
        path,
        revision,
        resolved_path: None,
        source_url: None,
        source_bytes_base64: None,
        source_raw_object_refs: Vec::new(),
        raw_object_refs: Vec::new(),
    })
}

fn required_string(value: &Value, fields: &[&str]) -> Result<String> {
    let mut current = value;
    for field in fields {
        current = current
            .get(*field)
            .ok_or_else(|| anyhow!("GitHub response lacks {}", fields.join(".")))?;
    }
    current
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "GitHub response field {} is not a non-empty string",
                fields.join(".")
            )
        })
}

fn required_sha(value: &Value, fields: &[&str]) -> Result<String> {
    let sha = required_string(value, fields)?;
    if !is_hex_revision(&sha, 40) {
        bail!(
            "GitHub response field {} is not a 40-hex revision",
            fields.join(".")
        );
    }
    Ok(sha)
}

fn required_identifier(value: &Value, fields: &[&str]) -> Result<String> {
    let mut current = value;
    for field in fields {
        current = current
            .get(*field)
            .ok_or_else(|| anyhow!("GitHub response lacks {}", fields.join(".")))?;
    }
    match current {
        Value::String(value) if !value.trim().is_empty() => Ok(value.clone()),
        Value::Number(value) if value.as_u64().is_some_and(|id| id > 0) => Ok(value.to_string()),
        _ => bail!(
            "GitHub response field {} is not a non-empty identifier",
            fields.join(".")
        ),
    }
}

fn optional_string(value: &Value, fields: &[&str]) -> Option<String> {
    let mut current = value;
    for field in fields {
        current = current.get(*field)?;
    }
    current
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn optional_revision(value: &Value, fields: &[&str]) -> Result<Option<String>> {
    let Some(revision) = optional_string(value, fields) else {
        return Ok(None);
    };
    if !is_hex_revision(&revision, 40) {
        bail!(
            "GitHub response field {} is not a 40-hex revision",
            fields.join(".")
        );
    }
    Ok(Some(revision))
}

fn required_u64(value: &Value, fields: &[&str]) -> Result<u64> {
    let mut current = value;
    for field in fields {
        current = current
            .get(*field)
            .ok_or_else(|| anyhow!("GitHub response lacks {}", fields.join(".")))?;
    }
    current
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("GitHub response field {} is not positive", fields.join(".")))
}

fn required_u32(value: &Value, fields: &[&str]) -> Result<u32> {
    let raw = required_u64(value, fields)?;
    u32::try_from(raw).with_context(|| {
        format!(
            "GitHub response field {} exceeds u32 run-attempt range",
            fields.join(".")
        )
    })
}

fn optional_u64(value: &Value, fields: &[&str]) -> Option<u64> {
    let mut current = value;
    for field in fields {
        current = current.get(*field)?;
    }
    current.as_u64().filter(|value| *value > 0)
}

fn optional_u32(value: &Value, fields: &[&str]) -> Result<Option<u32>> {
    let Some(raw) = optional_u64(value, fields) else {
        return Ok(None);
    };
    Ok(Some(u32::try_from(raw).with_context(|| {
        format!(
            "GitHub response field {} exceeds u32 run-attempt range",
            fields.join(".")
        )
    })?))
}

fn collection_id(repository: &str, suffix: &str) -> String {
    format!("{}--{}", safe_id(repository), safe_id(suffix))
}

fn validate_repository_slug(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("repository must be an owner/name slug with safe path characters");
    }
    Ok(())
}

fn safe_id(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-') {
            encoded.push(byte as char);
        } else if byte == b'_' {
            encoded.push_str("__");
        } else {
            encoded.push('_');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn acquisition_error_message(error: AcquisitionError) -> String {
    match error {
        AcquisitionError::InvalidRequest => "invalid request".to_owned(),
        AcquisitionError::EndpointViolation => "endpoint violation".to_owned(),
        AcquisitionError::SecretMetadata => "secret metadata".to_owned(),
        AcquisitionError::CredentialMaterialDetected => "credential material detected".to_owned(),
        AcquisitionError::Serialization => "serialization failure".to_owned(),
        AcquisitionError::StorageUnavailable => "raw storage unavailable".to_owned(),
        AcquisitionError::StorageRefused => "raw storage refused".to_owned(),
        AcquisitionError::StorageUnbound => "raw storage unbound".to_owned(),
        AcquisitionError::RawReferenceMismatch => "raw reference mismatch".to_owned(),
        AcquisitionError::RawDigestMismatch => "raw digest mismatch".to_owned(),
    }
}

fn utc_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::super::{
        sha256_digest, AcquisitionFuture, AcquisitionRequest, AcquisitionState,
        AcquisitionTransport, ApiKind, HttpMethod, PageState, RawObject, RawStorageError,
        TransportFailure, TransportResponse,
    };
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FixtureTransport {
        requests: Mutex<Vec<AcquisitionRequest>>,
        responses: Mutex<VecDeque<Result<TransportResponse, TransportFailure>>>,
    }

    impl FixtureTransport {
        fn with_responses(responses: Vec<Result<TransportResponse, TransportFailure>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }

        fn with_json_values(values: Vec<(&str, Value)>) -> Self {
            let responses = values
                .into_iter()
                .map(|(endpoint, value)| {
                    Ok(TransportResponse {
                        status: 200,
                        headers: BTreeMap::from([(
                            "content-type".to_owned(),
                            "application/json".to_owned(),
                        )]),
                        body: serde_json::to_vec(&value).expect("fixture JSON"),
                        effective_endpoint: endpoint.to_owned(),
                    })
                })
                .collect();
            Self::with_responses(responses)
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("fixture request lock").len()
        }
    }

    impl AcquisitionTransport for FixtureTransport {
        fn send<'a>(
            &'a self,
            request: AcquisitionRequest,
        ) -> AcquisitionFuture<'a, Result<TransportResponse, TransportFailure>> {
            self.requests
                .lock()
                .expect("fixture request lock")
                .push(request);
            let response = self
                .responses
                .lock()
                .expect("fixture response lock")
                .pop_front()
                .unwrap_or(Err(TransportFailure::Other));
            Box::pin(async move { response })
        }
    }

    #[derive(Default)]
    struct FixtureStore {
        bytes: BTreeMap<String, Vec<u8>>,
        refs: Vec<RawObjectRef>,
    }

    impl RawObjectStore for FixtureStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let digest = sha256_digest(&object.bytes);
            let original_digest = sha256_digest(&object.original_bytes);
            let storage_digest = digest
                .strip_prefix("sha256:")
                .expect("sha256 digest prefix");
            let reference = RawObjectRef {
                raw_id: object.raw_id.clone(),
                request_id: object.request_id.clone(),
                object_kind: object.object_kind.clone(),
                canonicalization: object.canonicalization.clone(),
                sha256: digest.clone(),
                byte_length: object.bytes.len() as u64,
                original_sha256: original_digest.clone(),
                original_byte_length: object.original_bytes.len() as u64,
                bytes_base64: BASE64.encode(&object.bytes),
                media_type: object.media_type.clone(),
                storage_ref: format!("sha256://{storage_digest}"),
                original_storage_ref: format!(
                    "sha256://{}",
                    original_digest
                        .strip_prefix("sha256:")
                        .expect("original sha256 digest prefix")
                ),
            };
            self.bytes.insert(storage_digest.to_owned(), object.bytes);
            self.refs.push(reference.clone());
            Ok(reference)
        }

        fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
            let digest = reference
                .storage_ref
                .strip_prefix("sha256://")
                .ok_or(RawStorageError::Unbound)?;
            let bytes = self.bytes.get(digest).ok_or(RawStorageError::Unbound)?;
            if sha256_digest(bytes) != reference.sha256
                || bytes.len() as u64 != reference.byte_length
                || BASE64.encode(bytes) != reference.bytes_base64
            {
                return Err(RawStorageError::Refused);
            }
            Ok(())
        }
    }

    #[test]
    fn failed_collection_is_retained_in_ledger_before_error() {
        let request = RequestRecord {
            request_id: "capture-0001".to_owned(),
            api: ApiKind::Rest,
            method: HttpMethod::Get,
            endpoint_or_operation: "https://api.github.com/items".to_owned(),
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
            response_raw_ref: None,
            error_raw_ref: None,
            state: AcquisitionState::TransportError,
            complete: false,
            truncation_reason: Some("transport failure".to_owned()),
        };
        let raw = RawObjectRef {
            raw_id: "capture-0001-error".to_owned(),
            request_id: request.request_id.clone(),
            object_kind: "items.error".to_owned(),
            canonicalization: "raw-bytes-v1".to_owned(),
            sha256: "sha256:error".to_owned(),
            byte_length: 0,
            original_sha256: "sha256:error".to_owned(),
            original_byte_length: 0,
            bytes_base64: String::new(),
            media_type: "application/json".to_owned(),
            storage_ref: "sha256://error".to_owned(),
            original_storage_ref: "sha256://error".to_owned(),
        };
        let result = CollectionResult {
            items: Vec::new(),
            requests: vec![request],
            raw_objects: vec![raw],
            state: AcquisitionState::TransportError,
            complete: false,
        };
        let mut ledger = Ledger::default();
        assert!(ledger.ingest(result).is_err());
        assert_eq!(ledger.requests.len(), 1);
        assert_eq!(ledger.raw_objects.len(), 1);
    }

    #[tokio::test]
    async fn production_dependency_binding_captures_tree_request_and_raw_identity() {
        let tree = serde_json::json!({
            "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "truncated": false,
            "tree": [{"path": "action.yml", "type": "blob"}]
        });
        let source = serde_json::json!({
            "path": "action.yml",
            "sha": "dddddddddddddddddddddddddddddddddddddddd",
            "encoding": "base64",
            "content": BASE64.encode(b"name: checkout\nruns:\n  using: composite\n  steps: []\n"),
            "url": "https://api.github.com/repos/actions/checkout/contents/action.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        let source_bytes = b"name: checkout\nruns:\n  using: composite\n  steps: []\n".to_vec();
        let transport = FixtureTransport::with_responses(vec![
            Ok(TransportResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    "application/json".to_owned(),
                )]),
                body: serde_json::to_vec(&tree).expect("tree fixture"),
                effective_endpoint: "https://api.github.com/repos/actions/checkout/git/trees/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?recursive=1".to_owned(),
            }),
            Ok(TransportResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    "application/json".to_owned(),
                )]),
                body: serde_json::to_vec(&source).expect("source metadata fixture"),
                effective_endpoint: "https://api.github.com/repos/actions/checkout/contents/action.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            }),
            Ok(TransportResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    "text/yaml".to_owned(),
                )]),
                body: source_bytes,
                effective_endpoint: "https://api.github.com/repos/actions/checkout/contents/action.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            }),
        ]);
        let mut store = FixtureStore::default();
        let auth = AuthIdentity::new(
            "fixture-auth",
            "github",
            Some("1".to_owned()),
            Some("fixture".to_owned()),
            BTreeSet::new(),
        );
        let manifest = ManifestRepository {
            repository: "tailrocks/example".to_owned(),
            ..ManifestRepository::default()
        };
        let mut ledger = Ledger::default();
        let dependencies = bind_workflow_dependencies(
            WorkflowDependencyContext {
                transport: &transport,
                store: &mut store,
                auth: &auth,
                manifest: &manifest,
                workflow_path: ".github/workflows/ci.yml",
                source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ledger: &mut ledger,
            },
            WorkflowDependencyGroups {
                reusable: Vec::new(),
                actions: vec![
                    LiveDependency {
                        kind: "action".to_owned(),
                        repository: "actions/checkout".to_owned(),
                        path: ".".to_owned(),
                        revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                        resolved_path: None,
                        source_url: None,
                        source_bytes_base64: None,
                        source_raw_object_refs: Vec::new(),
                        raw_object_refs: Vec::new(),
                    },
                    LiveDependency {
                        kind: "action".to_owned(),
                        repository: "actions/checkout".to_owned(),
                        path: "./".to_owned(),
                        revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                        resolved_path: None,
                        source_url: None,
                        source_bytes_base64: None,
                        source_raw_object_refs: Vec::new(),
                        raw_object_refs: Vec::new(),
                    },
                ],
                scanners: Vec::new(),
            },
        )
        .await
        .expect("dependency tree binding");
        assert_eq!(dependencies.actions.len(), 2);
        assert_eq!(dependencies.actions[0].raw_object_refs.len(), 1);
        assert_eq!(
            dependencies.actions[0].raw_object_refs,
            dependencies.actions[1].raw_object_refs
        );
        assert_eq!(
            dependencies.actions[0].resolved_path.as_deref(),
            Some("action.yml")
        );
        assert_eq!(
            dependencies.actions[0].source_url.as_deref(),
            Some(
                "https://api.github.com/repos/actions/checkout/contents/action.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
        );
        assert!(dependencies.actions[0].source_bytes_base64.is_some());
        assert_eq!(dependencies.actions[0].source_raw_object_refs.len(), 1);
        assert_eq!(
            dependencies.actions[0].source_raw_object_refs,
            dependencies.actions[1].source_raw_object_refs
        );
        assert_eq!(ledger.raw_objects.len(), 3);
        assert_eq!(ledger.raw_objects[0].object_kind, "workflow.dependency");
        assert_eq!(
            ledger.raw_objects[1].object_kind,
            "workflow.dependency.source"
        );
        assert_eq!(
            ledger.raw_objects[2].object_kind,
            "workflow.dependency.source"
        );
        let requests = transport.requests.lock().expect("fixture request lock");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[0].endpoint_or_operation,
            "https://api.github.com/repos/actions/checkout/git/trees/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?recursive=1"
        );
        assert_eq!(
            requests[0].endpoint_or_operation,
            ledger.requests[0].endpoint_or_operation
        );
        assert_eq!(
            requests[1].endpoint_or_operation,
            "https://api.github.com/repos/actions/checkout/contents/action.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            requests[2].accept.as_deref(),
            Some("application/vnd.github.raw+json")
        );
    }

    #[tokio::test]
    async fn dependency_cache_does_not_collapse_different_source_endpoints() {
        let revision = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tree = serde_json::json!({
            "sha": revision,
            "truncated": false,
            "tree": [{"path": "action.yml", "type": "blob"}]
        });
        let source_bytes = b"name: action\nruns:\n  using: composite\n  steps: []\n".to_vec();
        let source = |repository: &str| {
            serde_json::json!({
                "path": "action.yml",
                "sha": "dddddddddddddddddddddddddddddddddddddddd",
                "encoding": "base64",
                "content": BASE64.encode(&source_bytes),
                "url": format!(
                    "https://api.github.com/repos/{repository}/contents/action.yml?ref={revision}"
                )
            })
        };
        let response = |endpoint: String, body: Vec<u8>, content_type: &str| {
            Ok(TransportResponse {
                status: 200,
                headers: BTreeMap::from([("content-type".to_owned(), content_type.to_owned())]),
                body,
                effective_endpoint: endpoint,
            })
        };
        let repositories = ["actions/checkout", "actions/setup"];
        let mut responses = Vec::new();
        for repository in repositories {
            responses.push(response(
                format!(
                    "https://api.github.com/repos/{repository}/git/trees/{revision}?recursive=1"
                ),
                serde_json::to_vec(&tree).expect("tree fixture"),
                "application/json",
            ));
            responses.push(response(
                format!(
                    "https://api.github.com/repos/{repository}/contents/action.yml?ref={revision}"
                ),
                serde_json::to_vec(&source(repository)).expect("source fixture"),
                "application/json",
            ));
            responses.push(response(
                format!(
                    "https://api.github.com/repos/{repository}/contents/action.yml?ref={revision}"
                ),
                source_bytes.clone(),
                "text/yaml",
            ));
        }
        let transport = FixtureTransport::with_responses(responses);
        let mut store = FixtureStore::default();
        let auth = AuthIdentity::new(
            "fixture-auth",
            "github",
            Some("1".to_owned()),
            Some("fixture".to_owned()),
            BTreeSet::new(),
        );
        let manifest = ManifestRepository {
            repository: "tailrocks/example".to_owned(),
            ..ManifestRepository::default()
        };
        let mut ledger = Ledger::default();
        let dependency = |repository: &str| LiveDependency {
            kind: "action".to_owned(),
            repository: repository.to_owned(),
            path: ".".to_owned(),
            revision: revision.to_owned(),
            resolved_path: None,
            source_url: None,
            source_bytes_base64: None,
            source_raw_object_refs: Vec::new(),
            raw_object_refs: Vec::new(),
        };
        let dependencies = bind_workflow_dependencies(
            WorkflowDependencyContext {
                transport: &transport,
                store: &mut store,
                auth: &auth,
                manifest: &manifest,
                workflow_path: ".github/workflows/ci.yml",
                source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ledger: &mut ledger,
            },
            WorkflowDependencyGroups {
                reusable: Vec::new(),
                actions: repositories
                    .iter()
                    .map(|repository| dependency(repository))
                    .collect(),
                scanners: Vec::new(),
            },
        )
        .await
        .expect("different dependency endpoints bind independently");
        assert_eq!(dependencies.actions.len(), 2);
        assert_eq!(dependencies.actions[0].repository, "actions/checkout");
        assert_eq!(dependencies.actions[1].repository, "actions/setup");
        assert_ne!(
            dependencies.actions[0].source_url,
            dependencies.actions[1].source_url
        );
        assert_eq!(transport.request_count(), 6);
        assert_eq!(ledger.raw_objects.len(), 6);
    }

    #[tokio::test]
    async fn dependency_fetch_error_fails_closed_but_retains_failure_request() {
        let transport = FixtureTransport::with_responses(vec![Err(TransportFailure::Timeout)]);
        let mut store = FixtureStore::default();
        let auth = AuthIdentity::new(
            "fixture-auth",
            "github",
            Some("1".to_owned()),
            Some("fixture".to_owned()),
            BTreeSet::new(),
        );
        let manifest = ManifestRepository {
            repository: "tailrocks/example".to_owned(),
            ..ManifestRepository::default()
        };
        let mut ledger = Ledger::default();
        let result = bind_workflow_dependencies(
            WorkflowDependencyContext {
                transport: &transport,
                store: &mut store,
                auth: &auth,
                manifest: &manifest,
                workflow_path: ".github/workflows/ci.yml",
                source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ledger: &mut ledger,
            },
            WorkflowDependencyGroups {
                reusable: Vec::new(),
                actions: vec![LiveDependency {
                    kind: "action".to_owned(),
                    repository: "actions/checkout".to_owned(),
                    path: ".".to_owned(),
                    revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    resolved_path: None,
                    source_url: None,
                    source_bytes_base64: None,
                    source_raw_object_refs: Vec::new(),
                    raw_object_refs: Vec::new(),
                }],
                scanners: Vec::new(),
            },
        )
        .await;
        assert!(result.is_err());
        assert!(ledger.dependency_trees.is_empty());
        assert!(ledger.dependency_sources.is_empty());
        assert_eq!(ledger.requests.len(), 1);
        assert_eq!(ledger.requests[0].state, AcquisitionState::TransportError);
        assert!(ledger.raw_objects.is_empty());
    }

    #[test]
    fn workflow_dependency_parser_requires_revision_and_preserves_categories() {
        let yaml = r#"
on: [push]
jobs:
  build:
    uses: ./.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  scan:
    steps:
      - uses: github/codeql-action/init@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
      - uses: actions/checkout@cccccccccccccccccccccccccccccccccccccccc
"#;
        let (reusable, actions, scanners) = parse_workflow_dependencies(
            yaml,
            "tailrocks/example",
            "0123456789012345678901234567890123456789",
            &["raw".to_owned()],
        )
        .expect("dependencies");
        assert_eq!(reusable.len(), 1);
        assert_eq!(actions.len(), 1);
        assert_eq!(scanners.len(), 1);
        assert_eq!(
            reusable[0].revision,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let (_, local_actions, _) = parse_workflow_dependencies(
            "jobs: {build: {steps: [{uses: ./.github/actions/setup}]}}",
            "tailrocks/example",
            "0123456789012345678901234567890123456789",
            &["raw".to_owned()],
        )
        .expect("local action resolves to workflow source revision");
        assert_eq!(
            local_actions[0].revision,
            "0123456789012345678901234567890123456789"
        );
        assert!(parse_workflow_dependencies(
            "jobs: {build: {steps: [{uses: actions/checkout}]}}",
            "tailrocks/example",
            "0123456789012345678901234567890123456789",
            &["raw".to_owned()]
        )
        .is_err());
        assert!(parse_workflow_dependencies(
            "jobs: {build: {steps: [{uses: actions/checkout@v4}]}}",
            "tailrocks/example",
            "0123456789012345678901234567890123456789",
            &["raw".to_owned()]
        )
        .is_err());
    }

    #[test]
    fn dependency_tree_requires_untruncated_blob_target() {
        let tree = serde_json::json!({
            "truncated": false,
            "tree": [
                {"path": "action.yml", "type": "blob"},
                {"path": "docs/action.yml", "type": "tree"}
            ]
        });
        validate_dependency_tree(&tree, "actions/checkout", ".").expect("root action");
        assert!(validate_dependency_tree(&tree, "actions/checkout", "docs").is_err());
        let mut truncated = tree.clone();
        truncated["truncated"] = serde_json::json!(true);
        assert!(validate_dependency_tree(&truncated, "actions/checkout", ".").is_err());
    }

    #[test]
    fn artifacts_with_same_name_across_runs_keep_run_provenance() {
        let artifact = |artifact_id, run_id| LiveArtifact {
            artifact_id,
            run_id,
            run_attempt: Some(1),
            run_head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            name: "dist".to_owned(),
            digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            expired: Some(false),
            source_url: format!(
                "https://api.github.com/repos/tailrocks/example/actions/artifacts/{artifact_id}/zip"
            ),
            raw_object_refs: vec![format!("raw-{artifact_id}")],
        };
        let mut destination = Vec::new();
        merge_artifacts(&mut destination, vec![artifact(1, 10), artifact(2, 11)])
            .expect("same artifact name across runs is valid");
        assert_eq!(destination.len(), 2);
        assert!(merge_artifacts(&mut destination, vec![artifact(3, 10)]).is_err());
    }

    #[test]
    fn full_pr_identity_keeps_deleted_fork_as_missing_not_base_fallback() {
        let value = serde_json::json!({
            "number": 7,
            "state": "open",
            "draft": false,
            "user": {"login": "bot"},
            "author_association": "CONTRIBUTOR",
            "head": {"repo": null, "ref": "feature", "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "base": {"repo": {"full_name": "tailrocks/example"}, "ref": "main", "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
            "merge_commit_sha": null,
            "html_url": "https://github.com/tailrocks/example/pull/7"
        });
        let identity =
            parse_pull_request_identity(&value, vec!["raw".to_owned()]).expect("identity");
        assert_eq!(identity.head_repository, None);
        assert_eq!(identity.tested_merge_sha, None);
    }

    #[test]
    fn safe_ids_are_collision_free_for_path_delimiters() {
        assert_ne!(safe_id("owner/repo"), safe_id("owner-repo"));
        assert_ne!(
            collection_id("owner/repo", "a/b"),
            collection_id("owner/repo", "a-b")
        );
    }

    #[test]
    fn hostile_provenance_urls_are_rejected() {
        assert!(validate_workflow_attempt_url(
            "https://api.github.com/repos/tailrocks/velnor/actions/runs/7/attempts/1",
            "tailrocks/velnor",
            7,
            1,
        )
        .is_err());
        assert!(validate_workflow_attempt_url(
            "https://github.com.evil/tailrocks/velnor/actions/runs/7/attempts/1",
            "tailrocks/velnor",
            7,
            1,
        )
        .is_err());
        assert!(validate_job_url(
            "https://github.com/other/repo/runs/9",
            "tailrocks/velnor",
            7,
            8,
        )
        .is_err());
        assert!(validate_job_url(
            "https://github.com/tailrocks/velnor/actions/runs/7/job/9",
            "tailrocks/velnor",
            7,
            8,
        )
        .is_err());
        assert!(validate_artifact_url(
            "https://api.github.com/repos/other/repo/actions/artifacts/9/zip",
            "tailrocks/velnor",
            9,
        )
        .is_err());
        assert!(validate_artifact_url(
            "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9/zip",
            "tailrocks/velnor",
            8,
        )
        .is_err());
        assert!(parse_previous_attempt_url(
            "https://api.github.com/repos/tailrocks/velnor/actions/runs/7/attempts/1?token=secret",
            "tailrocks/velnor",
            7,
        )
        .is_err());
        let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut content = serde_json::json!({
            "path": ".github/workflows/ci.yml",
            "sha": source_sha,
            "url": "https://api.github.com/repos/tailrocks/velnor/contents/.github/workflows/ci.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        assert!(validate_workflow_content(
            &content,
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            source_sha
        )
        .is_ok());
        content["url"] = serde_json::json!(
            "https://api.github.com/repos/tailrocks/velnor/contents/.github/workflows/ci.yml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&token=secret"
        );
        assert!(validate_workflow_content(
            &content,
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            source_sha
        )
        .is_err());
    }

    #[test]
    fn ruleset_app_identity_is_numeric_and_duplicates_fail_closed() {
        let duplicate = serde_json::json!({
            "rules": [{
                "parameters": {
                    "required_status_checks": [
                        {"context": "build", "integration_id": 123},
                        {"context": "build", "integration_id": 123}
                    ]
                }
            }]
        });
        assert!(parse_ruleset_checks(&duplicate, 1, vec![]).is_err());
        let slug = serde_json::json!({
            "rules": [{
                "parameters": {
                    "required_status_checks": [{"context": "scan", "app": "sonarcloud"}]
                }
            }]
        });
        let checks = parse_ruleset_checks(&slug, 1, vec![]).expect("ruleset checks");
        assert_eq!(checks[0].app_id, None);
    }

    #[test]
    fn check_identity_requires_api_bound_suite_source_and_repository() {
        let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let value = serde_json::json!({
            "id": 17,
            "name": "ci",
            "head_sha": source_sha,
            "repository": {"full_name": "tailrocks/example"},
            "check_suite": {
                "id": 9,
                "workflow_run": {
                    "id": 11,
                    "head_sha": source_sha,
                    "run_attempt": 1,
                    "event": "pull_request"
                }
            },
            "app": {"id": 123, "slug": "github-actions"},
            "status": "completed",
            "conclusion": "success",
            "url": "https://api.github.com/repos/tailrocks/example/check-runs/17",
            "html_url": "https://github.com/tailrocks/example/runs/17"
        });
        let check = parse_check(
            &value,
            "tailrocks/example",
            source_sha,
            vec!["raw".to_owned()],
            9,
        )
        .expect("API-bound check");
        assert_eq!(check.check_suite_id, Some(9));
        assert_eq!(check.workflow_run_id, Some(11));
        assert_eq!(check.app_id.as_deref(), Some("123"));

        let mut wrong_source = value.clone();
        wrong_source["head_sha"] = serde_json::json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(parse_check(&wrong_source, "tailrocks/example", source_sha, vec![], 9,).is_err());

        let mut slug_only = value.clone();
        slug_only["app"] = serde_json::json!({"slug": "sonarcloud"});
        let slug_check = parse_check(&slug_only, "tailrocks/example", source_sha, vec![], 9)
            .expect("slug check remains observed but unbound");
        assert_eq!(slug_check.app_id, None);
        let mut external_id = value.clone();
        external_id["external_id"] = serde_json::json!("123");
        let external_id_check =
            parse_check(&external_id, "tailrocks/example", source_sha, vec![], 9)
                .expect("external check remains observed but job-unbound");
        assert_eq!(external_id_check.job_id, None);
    }

    #[test]
    fn job_identity_requires_api_check_run_binding() {
        let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let value = serde_json::json!({
            "id": 8,
            "run_id": 7,
            "run_attempt": 2,
            "head_sha": source_sha,
            "name": "build",
            "status": "completed",
            "conclusion": "success",
            "url": "https://api.github.com/repos/tailrocks/example/actions/jobs/8",
            "run_url": "https://api.github.com/repos/tailrocks/example/actions/runs/7",
            "html_url": "https://github.com/tailrocks/example/runs/7/jobs/8",
            "check_run_url": "https://api.github.com/repos/tailrocks/example/check-runs/17"
        });
        let job = parse_job(
            &value,
            "pull_request",
            "tailrocks/example",
            1,
            7,
            2,
            source_sha,
            vec!["raw".to_owned()],
        )
        .expect("API-bound job");
        assert_eq!(job.job_id, 8);
        assert_eq!(job.check_run_id, 17);
        assert!(validate_unique_jobs(&[job.clone(), job], 7, 2).is_err());

        let mut hostile = value.clone();
        hostile["check_run_url"] =
            serde_json::json!("https://api.github.com.evil/repos/tailrocks/example/check-runs/17");
        assert!(parse_job(
            &hostile,
            "pull_request",
            "tailrocks/example",
            1,
            7,
            2,
            source_sha,
            vec![]
        )
        .is_err());
    }

    #[test]
    fn checkout_observation_does_not_promote_api_head_to_checkout_proof() {
        let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        let observation =
            LiveCheckoutObservation::api_head_only(sha.clone(), vec!["raw".to_owned()]);
        assert_eq!(observation.api_head_sha, sha);
        assert_eq!(observation.api_raw_object_refs, vec!["raw"]);
        assert_eq!(observation.actual_checkout_sha(), None);
        assert!(observation.proof.is_none());
    }

    #[test]
    fn workflow_binding_rejects_conflicting_checkout_proofs() {
        let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let execution = |checkout_sha: &str| LiveExecution {
            run_id: 7,
            run_attempt: 1,
            workflow_path: ".github/workflows/ci.yml".to_owned(),
            workflow_revision: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            event: "pull_request".to_owned(),
            source_sha: source_sha.to_owned(),
            checkout: LiveCheckoutObservation {
                api_head_sha: source_sha.to_owned(),
                api_raw_object_refs: vec!["api".to_owned()],
                proof: Some(LiveCheckoutProof {
                    checkout_sha: checkout_sha.to_owned(),
                    source_kind: "runner-attestation".to_owned(),
                    raw_object_refs: vec!["proof".to_owned()],
                }),
            },
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            source_url: "https://github.com/tailrocks/example/actions/runs/7/attempts/1".to_owned(),
            jobs: Vec::new(),
            raw_object_refs: vec!["raw".to_owned()],
        };
        assert!(workflow_bindings(&[
            execution(source_sha),
            execution("cccccccccccccccccccccccccccccccccccccccc")
        ])
        .is_err());
    }

    #[test]
    fn check_suite_identity_requires_api_source_binding() {
        let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let value = serde_json::json!({
            "id": 9,
            "head_sha": source_sha,
            "repository": {"full_name": "tailrocks/example"},
            "url": "https://api.github.com/repos/tailrocks/example/check-suites/9",
            "check_runs_url": "https://api.github.com/repos/tailrocks/example/check-suites/9/check-runs"
        });
        validate_check_suite(&value, "tailrocks/example", source_sha, 9).expect("API-bound suite");

        let mut wrong_source = value.clone();
        wrong_source["head_sha"] = serde_json::json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(validate_check_suite(&wrong_source, "tailrocks/example", source_sha, 9).is_err());
    }

    #[test]
    fn access_proof_requires_read_permission_fields_and_endpoint_observations() {
        let repository = serde_json::json!({
            "permissions": {"pull": false},
            "private": false,
            "visibility": "public",
            "owner": {"login": "tailrocks"}
        });
        let shape_gaps = access_gaps(&repository);
        assert_eq!(shape_gaps, vec!["repository.permissions.pull"]);
        let endpoint_gaps = access_endpoint_gaps(&[], "tailrocks/example");
        assert!(endpoint_gaps.iter().any(|gap| gap == "api.workflow_jobs"));
        assert!(endpoint_gaps.iter().any(|gap| gap == "api.check_runs"));
    }

    #[test]
    fn artifact_parser_requires_run_binding_and_digest() {
        let value = serde_json::json!({
            "id": 9,
            "name": "bundle",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "expired": false,
            "url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9",
            "archive_download_url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9/zip",
            "workflow_run": {"id": 7, "repository_id": 1, "head_sha": "cccccccccccccccccccccccccccccccccccccccc"}
        });
        let artifact = parse_artifact(
            &value,
            "tailrocks/velnor",
            1,
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec!["raw".to_owned()],
        )
        .expect("artifact");
        assert_eq!(artifact.run_id, 7);
        assert_eq!(artifact.run_attempt, None);
        let mut foreign_repository = value.clone();
        foreign_repository["workflow_run"]["repository_id"] = serde_json::json!(2);
        assert!(parse_artifact(
            &foreign_repository,
            "tailrocks/velnor",
            1,
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec![]
        )
        .is_err());
        let mut missing_repository = value.clone();
        missing_repository["workflow_run"] = serde_json::json!({
            "id": 7,
            "head_sha": "cccccccccccccccccccccccccccccccccccccccc"
        });
        assert!(parse_artifact(
            &missing_repository,
            "tailrocks/velnor",
            1,
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec![]
        )
        .is_err());
        assert!(parse_artifact(
            &serde_json::json!({
                "id": 9,
                "name": "bundle",
                "url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9",
                "archive_download_url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9/zip",
                "workflow_run": {"id": 7, "repository_id": 1, "head_sha": "cccccccccccccccccccccccccccccccccccccccc"}
            }),
            "tailrocks/velnor",
            1,
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec![]
        )
        .is_err());
    }

    #[test]
    fn workflow_run_identity_binds_repository_urls_and_attempts() {
        let source_sha = "cccccccccccccccccccccccccccccccccccccccc";
        let run = serde_json::json!({
            "id": 7,
            "path": ".github/workflows/ci.yml@main",
            "head_sha": source_sha,
            "run_attempt": 2,
            "url": "https://api.github.com/repos/tailrocks/velnor/actions/runs/7",
            "html_url": "https://github.com/tailrocks/velnor/actions/runs/7",
            "repository": {"id": 1, "full_name": "tailrocks/velnor"}
        });
        validate_workflow_run(&run, "tailrocks/velnor", 1, 7, source_sha, None)
            .expect("workflow run identity");
        assert_eq!(
            workflow_path_from_run(&run).expect("workflow path"),
            ".github/workflows/ci.yml"
        );
        let attempt = serde_json::json!({
            "id": 7,
            "path": ".github/workflows/ci.yml@main",
            "head_sha": source_sha,
            "run_attempt": 2,
            "url": "https://api.github.com/repos/tailrocks/velnor/actions/runs/7/attempts/2",
            "html_url": "https://github.com/tailrocks/velnor/actions/runs/7/attempts/2",
            "repository": {"id": 1, "full_name": "tailrocks/velnor"}
        });
        validate_workflow_run(&attempt, "tailrocks/velnor", 1, 7, source_sha, Some(2))
            .expect("workflow attempt identity");
        let mut foreign = run.clone();
        foreign["repository"]["id"] = serde_json::json!(2);
        assert!(
            validate_workflow_run(&foreign, "tailrocks/velnor", 1, 7, source_sha, None).is_err()
        );
    }
}
