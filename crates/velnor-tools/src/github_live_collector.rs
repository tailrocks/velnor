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
    collect_rest, github_check_suite_runs_request, github_check_suites_request,
    github_open_pull_requests_request, github_single_object_request,
    github_workflow_artifacts_request, github_workflow_attempt_jobs_request,
    github_workflow_attempt_request, github_workflow_runs_request, AcquisitionError, AuthIdentity,
    CollectionResult, IdentityReconciliation, RawObjectRef, RawObjectStore, RequestRecord,
    RestCollectionRequest, RevisionIdentity,
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
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveWorkflow {
    pub path: String,
    pub revision: String,
    pub source_sha: String,
    pub events: Vec<String>,
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
    pub run_id: u64,
    pub run_attempt: u32,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub event: String,
    pub source_sha: Option<String>,
    pub actual_checkout_sha: Option<String>,
    pub source_url: String,
    pub raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveArtifact {
    pub artifact_id: u64,
    pub run_id: u64,
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
    pub actual_checkout_sha: Option<String>,
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
    pub check_suite_id: Option<u64>,
    pub check_run_id: u64,
    pub workflow_run_id: Option<u64>,
    pub job_id: Option<u64>,
    pub run_attempt: Option<u32>,
    pub source_sha: String,
    pub actual_checkout_sha: Option<String>,
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
    pub actual_checkout_sha: Option<String>,
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

#[derive(Default)]
struct Ledger {
    requests: Vec<RequestRecord>,
    raw_objects: Vec<RawObjectRef>,
    request_ids: BTreeSet<String>,
    raw_ids: BTreeSet<String>,
}

impl Ledger {
    fn ingest(&mut self, result: CollectionResult) -> Result<Vec<Value>> {
        if !result.complete {
            bail!(
                "GitHub collection page set is incomplete: {:?}",
                result.state
            );
        }
        for request in &result.requests {
            if !self.request_ids.insert(request.request_id.clone()) {
                bail!("duplicate acquisition request id {}", request.request_id);
            }
        }
        for raw in &result.raw_objects {
            if !self.raw_ids.insert(raw.raw_id.clone()) {
                bail!("duplicate acquisition raw id {}", raw.raw_id);
            }
        }
        self.requests.extend(result.requests);
        self.raw_objects.extend(result.raw_objects);
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
    mut auth: AuthIdentity,
    manifest: &ManifestDocument,
    snapshot_id: impl Into<String>,
) -> Result<LiveCollection>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let snapshot_id = snapshot_id.into();
    validate_manifest_scope(manifest, &snapshot_id)?;
    let observed_at_utc = utc_now();
    let mut ledger = Ledger::default();
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
    ledger: &mut Ledger,
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
    ledger: &mut Ledger,
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
    ledger: &mut Ledger,
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
    for value in values {
        let number = required_u64(&value, &["number"])?;
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
    ledger: &mut Ledger,
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
            &format!("{phase}.repository"),
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
            &format!("{phase}.default_branch.commit"),
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
    ledger: &mut Ledger,
) -> Result<LiveRepository>
where
    T: super::AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    let raw_start = ledger.raw_objects.len();
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
    let workflows = collect_workflows(
        transport,
        store,
        auth,
        manifest,
        &default_branch_sha,
        ledger,
    )
    .await?;
    let identities =
        collect_pr_identities(transport, store, auth, manifest, "inventory", ledger).await?;
    let mut open_prs = Vec::with_capacity(identities.len());
    let mut artifacts = Vec::new();
    for identity in identities {
        let (executions, checks, run_artifacts) = collect_pr_execution_facts(
            transport, store, auth, manifest, &workflows, &identity, ledger,
        )
        .await?;
        merge_artifacts(&mut artifacts, run_artifacts)?;
        let workflow_bindings = workflow_bindings(&executions);
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
        &workflows,
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
        access_state: access_state(&repository_value),
        access_gaps: access_gaps(&repository_value),
        raw_object_refs: ledger.raw_ids_since(raw_start),
    })
}

async fn collect_rulesets<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    ledger: &mut Ledger,
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
    for value in values {
        let id = required_u64(&value, &["id"])?;
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
    ledger: &mut Ledger,
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
    for value in values {
        let path = required_string(&value, &["path"])?;
        let encoded_path = path.replace(' ', "%20");
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
        let revision = required_sha(&content, &["sha"])?;
        let source_text = decode_workflow_content(&content)?;
        let events = parse_workflow_events(&source_text)?;
        let (reusable_workflows, actions, scanners) = parse_workflow_dependencies(
            &source_text,
            &manifest.repository,
            source_sha,
            &content_raw_ids,
        )?;
        let mut raw_ids = list_raw_ids.clone();
        raw_ids.extend(content_raw_ids);
        workflows.push(LiveWorkflow {
            path,
            revision,
            source_sha: source_sha.to_owned(),
            events,
            reusable_workflows,
            actions,
            scanners,
            raw_object_refs: raw_ids,
        });
    }
    Ok(workflows)
}

async fn collect_pr_execution_facts<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    manifest: &ManifestRepository,
    workflows: &[LiveWorkflow],
    identity: &LivePullRequestIdentity,
    ledger: &mut Ledger,
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
    workflows: &[LiveWorkflow],
    source_sha: &str,
    collection_prefix: &str,
    ledger: &mut Ledger,
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
            continue;
        }
        let workflow_path = required_string(&run, &["path"])?;
        let workflow = workflows
            .iter()
            .find(|workflow| workflow.path == workflow_path)
            .ok_or_else(|| anyhow!("run {run_id} references unknown workflow {workflow_path}"))?;
        let latest_attempt = required_u64(&run, &["run_attempt"])? as u32;
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
        let run_artifacts = artifact_values
            .iter()
            .map(|artifact| {
                parse_artifact(
                    artifact,
                    &manifest.repository,
                    run_id,
                    &run_source_sha,
                    artifact_raw_ids.clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
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
            let attempt_path = required_string(&attempt, &["path"])?;
            if attempt_path != workflow_path {
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
                        run_id,
                        attempt_number,
                        &run_source_sha,
                        jobs_raw_ids.clone(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let mut raw_ids = run_list_raw_ids.clone();
            raw_ids.extend(attempt_raw_ids);
            raw_ids.extend(jobs_raw_ids);
            raw_ids.extend(artifact_raw_ids.clone());
            raw_ids.sort();
            raw_ids.dedup();
            executions.push(LiveExecution {
                run_id,
                run_attempt: attempt_number,
                workflow_path: workflow_path.clone(),
                workflow_revision: workflow.revision.clone(),
                event,
                source_sha: run_source_sha.clone(),
                actual_checkout_sha: None,
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
    ledger: &mut Ledger,
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
        let attempt_number = required_u64(&attempt, &["run_attempt"])? as u32;
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
    ledger: &mut Ledger,
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
    for suite in suites {
        let suite_id = required_u64(&suite, &["id"])?;
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
            let mut check = parse_check(&run, source_sha, raw_ids, suite_id)?;
            if check.check_suite_id.is_none() {
                check.check_suite_id = Some(suite_id);
            }
            checks.push(check);
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

fn workflow_bindings(executions: &[LiveExecution]) -> Vec<LiveWorkflowBinding> {
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
            actual_checkout_sha: execution.actual_checkout_sha.clone(),
            run_ids: Vec::new(),
            raw_object_refs: Vec::new(),
        });
        entry.run_ids.push(execution.run_id);
        if entry.actual_checkout_sha.is_none() {
            entry.actual_checkout_sha = execution.actual_checkout_sha.clone();
        }
        entry
            .raw_object_refs
            .extend(execution.raw_object_refs.clone());
    }
    grouped
        .into_values()
        .map(|mut binding| {
            binding.run_ids.sort();
            binding.run_ids.dedup();
            binding.raw_object_refs.sort();
            binding.raw_object_refs.dedup();
            binding
        })
        .collect()
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

fn parse_job(
    value: &Value,
    event: &str,
    repository: &str,
    run_id: u64,
    run_attempt: u32,
    source_sha: &str,
    raw_object_refs: Vec<String>,
) -> Result<LiveJob> {
    let job_run_id = required_u64(value, &["run_id"])?;
    if job_run_id != run_id {
        bail!("job belongs to run {job_run_id}, expected {run_id}");
    }
    let job_attempt = required_u64(value, &["run_attempt"])? as u32;
    if job_attempt != run_attempt {
        bail!("job belongs to attempt {job_attempt}, expected {run_attempt}");
    }
    let job_source_sha = required_sha(value, &["head_sha"])?;
    if job_source_sha != source_sha {
        bail!("job head SHA differs from workflow run");
    }
    let source_url = required_string(value, &["html_url"])?;
    validate_job_url(&source_url, repository, run_id)?;
    Ok(LiveJob {
        job_id: required_u64(value, &["id"])?,
        run_id: job_run_id,
        run_attempt: job_attempt,
        name: required_string(value, &["name"])?,
        status: required_string(value, &["status"])?,
        conclusion: optional_string(value, &["conclusion"]),
        event: event.to_owned(),
        source_sha: Some(job_source_sha),
        actual_checkout_sha: None,
        source_url,
        raw_object_refs,
    })
}

fn parse_artifact(
    value: &Value,
    repository: &str,
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
    let source_url = required_string(value, &["archive_download_url"])?;
    validate_artifact_url(&source_url, repository, run_id)?;
    let digest = required_string(value, &["digest"])?;
    if !is_digest(&digest) {
        bail!(
            "artifact {} has malformed digest",
            required_u64(value, &["id"])?
        );
    }
    Ok(LiveArtifact {
        artifact_id: required_u64(value, &["id"])?,
        run_id: artifact_run_id,
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
        if let Some(existing) = destination
            .iter_mut()
            .find(|existing| existing.artifact_id == artifact.artifact_id)
        {
            if existing.run_id != artifact.run_id
                || existing.run_head_sha != artifact.run_head_sha
                || existing.name != artifact.name
                || existing.digest != artifact.digest
                || existing.expired != artifact.expired
                || existing.source_url != artifact.source_url
            {
                bail!(
                    "artifact {} has conflicting observations",
                    artifact.artifact_id
                );
            }
            existing.raw_object_refs.extend(artifact.raw_object_refs);
            existing.raw_object_refs.sort();
            existing.raw_object_refs.dedup();
        } else {
            destination.push(artifact);
        }
    }
    destination.sort_by_key(|artifact| artifact.artifact_id);
    Ok(())
}

fn access_state(repository: &Value) -> String {
    if repository
        .get("permissions")
        .and_then(Value::as_object)
        .is_some()
    {
        "observed".to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn access_gaps(repository: &Value) -> Vec<String> {
    ["permissions", "private", "visibility", "owner"]
        .into_iter()
        .filter(|field| repository.get(*field).is_none())
        .map(str::to_owned)
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
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("api.github.com")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != format!("/repos/{repository}/contents/{path}")
        || parsed
            .query_pairs()
            .find(|(key, _)| key == "ref")
            .is_none_or(|(_, value)| value != source_sha)
    {
        bail!("workflow content URL is not bound to requested repository/path/ref");
    }
    Ok(())
}

fn validate_job_url(value: &str, repository: &str, _run_id: u64) -> Result<()> {
    let parsed = parse_safe_url(value, "github.com")?;
    let prefix = format!("/{repository}/");
    if !parsed.path().starts_with(&prefix) {
        bail!("job URL is not bound to repository {repository}");
    }
    Ok(())
}

fn validate_artifact_url(value: &str, repository: &str, _run_id: u64) -> Result<()> {
    let parsed = parse_safe_url(value, "api.github.com")?;
    let prefix = format!("/repos/{repository}/actions/artifacts/");
    if !parsed.path().starts_with(&prefix) {
        bail!("artifact URL is not bound to repository {repository}");
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

fn validate_external_url(value: &str) -> Result<()> {
    let parsed = Url::parse(value).context("parse check source URL")?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        bail!("check source URL has an unsafe origin or components");
    }
    Ok(())
}

fn parse_check(
    value: &Value,
    source_sha: &str,
    raw_ids: Vec<String>,
    suite_id: u64,
) -> Result<LiveCheck> {
    let check_suite_id = optional_u64(value, &["check_suite", "id"]).or(Some(suite_id));
    let workflow_run_id = optional_u64(value, &["check_suite", "workflow_run", "id"]);
    let app_id = value.get("app").and_then(|app| {
        app.get("id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .or_else(|| app.get("slug").and_then(Value::as_str).map(str::to_owned))
    });
    let job_id = optional_string(value, &["external_id"]).and_then(|id| id.parse().ok());
    let source_url = required_string(value, &["details_url"])
        .or_else(|_| required_string(value, &["html_url"]))?;
    validate_external_url(&source_url)?;
    Ok(LiveCheck {
        context: required_string(value, &["name"])?,
        app_id,
        check_suite_id,
        check_run_id: required_u64(value, &["id"])?,
        workflow_run_id,
        job_id,
        run_attempt: optional_u64(value, &["check_suite", "workflow_run", "run_attempt"])
            .map(|value| value as u32),
        source_sha: source_sha.to_owned(),
        actual_checkout_sha: None,
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
    raw_ids: &[String],
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
        raw_object_refs: raw_ids.to_vec(),
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

fn optional_u64(value: &Value, fields: &[&str]) -> Option<u64> {
    let mut current = value;
    for field in fields {
        current = current.get(*field)?;
    }
    current.as_u64().filter(|value| *value > 0)
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
    use super::*;

    #[test]
    fn workflow_dependency_parser_requires_revision_and_preserves_categories() {
        let yaml = r#"
on: [push]
jobs:
  build:
    uses: ./.github/workflows/reusable.yml@abc123
  scan:
    steps:
      - uses: github/codeql-action/init@v3
      - uses: actions/checkout@v4
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
        assert_eq!(reusable[0].revision, "abc123");
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
        )
        .is_err());
        assert!(validate_artifact_url(
            "https://api.github.com/repos/other/repo/actions/artifacts/9/zip",
            "tailrocks/velnor",
            7,
        )
        .is_err());
        assert!(parse_previous_attempt_url(
            "https://api.github.com/repos/tailrocks/velnor/actions/runs/7/attempts/1?token=secret",
            "tailrocks/velnor",
            7,
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
    fn artifact_parser_requires_run_binding_and_digest() {
        let value = serde_json::json!({
            "id": 9,
            "name": "bundle",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "expired": false,
            "archive_download_url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9/zip",
            "workflow_run": {"id": 7, "head_sha": "cccccccccccccccccccccccccccccccccccccccc"}
        });
        let artifact = parse_artifact(
            &value,
            "tailrocks/velnor",
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec!["raw".to_owned()],
        )
        .expect("artifact");
        assert_eq!(artifact.run_id, 7);
        assert!(parse_artifact(
            &serde_json::json!({
                "id": 9,
                "name": "bundle",
                "archive_download_url": "https://api.github.com/repos/tailrocks/velnor/actions/artifacts/9/zip",
                "workflow_run": {"id": 7, "head_sha": "cccccccccccccccccccccccccccccccccccccccc"}
            }),
            "tailrocks/velnor",
            7,
            "cccccccccccccccccccccccccccccccccccccccc",
            vec![]
        )
        .is_err());
    }
}
