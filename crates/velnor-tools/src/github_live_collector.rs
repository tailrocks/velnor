//! Live GitHub facts collector.
//!
//! This layer performs the read-only traversal that the old `evidence_live`
//! snapshot collector did not retain: every page is acquired through the
//! provenance ledger, workflow attempts are expanded before jobs are read,
//! check suites are expanded before check runs are read, and artifacts are
//! traversed for every attempt.  It intentionally keeps optional provider
//! identities optional; the later G0 mapper must reject them rather than
//! inventing workflow/job associations for external checks.

use super::{
    collect_rest, github_check_suite_runs_request, github_check_suites_request,
    github_open_pull_requests_request, github_single_object_request,
    github_workflow_artifacts_request, github_workflow_attempt_jobs_request,
    github_workflow_attempts_request, github_workflow_runs_request, AcquisitionError, AuthIdentity,
    CollectionResult, IdentityReconciliation, RawObjectRef, RawObjectStore, RequestRecord,
    RestCollectionRequest, RevisionIdentity,
};
use crate::evidence_check::{ManifestDocument, ManifestRepository};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
    pub app_id: String,
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
    pub name: String,
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
    pub artifacts: Vec<LiveArtifact>,
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
    pub main_executions: Vec<LiveExecution>,
    pub main_checks: Vec<LiveCheck>,
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
    if snapshot_id.trim().is_empty() || manifest.repositories.len() != 32 {
        bail!("live collector requires the reviewed 32-repository manifest");
    }
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
        identities.push(parse_pull_request_identity(&detail, raw_ids)?);
    }
    Ok(identities)
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
    let default_branch_sha = required_string(&commit, &["sha"])?;
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
    for identity in identities {
        let (executions, checks) = collect_pr_execution_facts(
            transport, store, auth, manifest, &workflows, &identity, ledger,
        )
        .await?;
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
    let main_executions = collect_executions_for_source(
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
        default_branch,
        default_branch_sha,
        rulesets,
        workflows,
        open_prs,
        main_executions,
        main_checks,
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
        rulesets.push(LiveRuleset {
            ruleset_id: id,
            name,
            source_url: format!(
                "https://api.github.com/repos/{}/rulesets/{id}",
                manifest.repository
            ),
            complete: true,
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
        let revision = required_string(&content, &["sha"])?;
        let source_text = decode_workflow_content(&content)?;
        let events = parse_workflow_events(&source_text)?;
        let (reusable_workflows, actions, scanners) =
            parse_workflow_dependencies(&source_text, &manifest.repository, &content_raw_ids)?;
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
) -> Result<(Vec<LiveExecution>, Vec<LiveCheck>)>
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
    for source_sha in sources {
        executions.extend(
            collect_executions_for_source(
                transport,
                store,
                auth,
                manifest,
                workflows,
                &source_sha,
                &format!("pr-{}", identity.number),
                ledger,
            )
            .await?,
        );
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
    Ok((executions, checks))
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
) -> Result<Vec<LiveExecution>>
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
    for run in runs {
        let run_id = required_u64(&run, &["id"])?;
        let run_source_sha = required_string(&run, &["head_sha"])?;
        if run_source_sha != source_sha {
            continue;
        }
        let workflow_path = required_string(&run, &["path"])?;
        let workflow = workflows
            .iter()
            .find(|workflow| workflow.path == workflow_path)
            .ok_or_else(|| anyhow!("run {run_id} references unknown workflow {workflow_path}"))?;
        let (attempt_values, attempt_raw_ids) = collect_items(
            transport,
            store,
            auth,
            ledger,
            github_workflow_attempts_request(
                collection_id(&manifest.repository, &format!("run-{run_id}-attempts")),
                &manifest.repository,
                run_id,
            ),
        )
        .await?;
        if attempt_values.is_empty() {
            bail!("workflow run {run_id} returned no attempts");
        }
        for attempt in attempt_values {
            let attempt_number = required_u64(&attempt, &["run_attempt"])? as u32;
            let attempt_run_id = optional_u64(&attempt, &["id"]).unwrap_or(run_id);
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
                        &required_string(&run, &["event"])?,
                        jobs_raw_ids.clone(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let (artifacts, artifacts_raw_ids) = collect_items(
                transport,
                store,
                auth,
                ledger,
                github_workflow_artifacts_request(
                    collection_id(
                        &manifest.repository,
                        &format!("run-{run_id}-attempt-{attempt_number}-artifacts"),
                    ),
                    &manifest.repository,
                    run_id,
                ),
            )
            .await?;
            let live_artifacts = artifacts
                .iter()
                .map(|artifact| parse_artifact(artifact, artifacts_raw_ids.clone()))
                .collect::<Result<Vec<_>>>()?;
            let mut raw_ids = run_list_raw_ids.clone();
            raw_ids.extend(attempt_raw_ids.clone());
            raw_ids.extend(jobs_raw_ids);
            raw_ids.extend(artifacts_raw_ids);
            raw_ids.sort();
            raw_ids.dedup();
            let actual_checkout_sha = live_jobs
                .iter()
                .find_map(|job| job.actual_checkout_sha.clone());
            executions.push(LiveExecution {
                run_id: attempt_run_id,
                run_attempt: attempt_number,
                workflow_path: workflow_path.clone(),
                workflow_revision: workflow.revision.clone(),
                event: required_string(&run, &["event"])?,
                source_sha: run_source_sha.clone(),
                actual_checkout_sha,
                status: required_string(&attempt, &["status"])
                    .or_else(|_| required_string(&run, &["status"]))?,
                conclusion: optional_string(&attempt, &["conclusion"])
                    .or_else(|| optional_string(&run, &["conclusion"])),
                source_url: required_string(&run, &["html_url"])?,
                jobs: live_jobs,
                artifacts: live_artifacts,
                raw_object_refs: raw_ids,
            });
        }
    }
    Ok(executions)
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
    checks.dedup_by_key(|check| check.check_run_id);
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
        head_sha: required_string(value, &["head", "sha"])?,
        base_repository: optional_string(value, &["base", "repo", "full_name"]),
        base_ref: required_string(value, &["base", "ref"])?,
        base_sha: required_string(value, &["base", "sha"])?,
        tested_merge_sha: optional_string(value, &["merge_commit_sha"]),
        merge_group_sha: optional_string(value, &["merge_group", "sha"]),
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
        bail!("ruleset {ruleset_id} lacks rules array");
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
                .and_then(|value| {
                    value
                        .as_i64()
                        .map(|id| id.to_string())
                        .or_else(|| value.as_str().map(str::to_owned))
                })
                .or_else(|| check.get("app").and_then(Value::as_str).map(str::to_owned))
                .ok_or_else(|| {
                    anyhow!("ruleset {ruleset_id} check {context} lacks app identity")
                })?;
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
    checks.dedup_by(|left, right| left.context == right.context && left.app_id == right.app_id);
    Ok(checks)
}

fn parse_job(value: &Value, event: &str, raw_object_refs: Vec<String>) -> Result<LiveJob> {
    Ok(LiveJob {
        job_id: required_u64(value, &["id"])?,
        name: required_string(value, &["name"])?,
        status: required_string(value, &["status"])?,
        conclusion: optional_string(value, &["conclusion"]),
        event: event.to_owned(),
        source_sha: optional_string(value, &["head_sha"]),
        actual_checkout_sha: optional_string(value, &["head_sha"]),
        source_url: required_string(value, &["html_url"])?,
        raw_object_refs,
    })
}

fn parse_artifact(value: &Value, raw_object_refs: Vec<String>) -> Result<LiveArtifact> {
    Ok(LiveArtifact {
        artifact_id: required_u64(value, &["id"])?,
        name: required_string(value, &["name"])?,
        expired: value.get("expired").and_then(Value::as_bool),
        source_url: required_string(value, &["archive_download_url"])?,
        raw_object_refs,
    })
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
        actual_checkout_sha: optional_string(value, &["check_suite", "head_sha"]),
        event: optional_string(value, &["check_suite", "workflow_run", "event"]),
        status: required_string(value, &["status"])?,
        conclusion: optional_string(value, &["conclusion"]),
        source_url: required_string(value, &["details_url"])
            .or_else(|_| required_string(value, &["html_url"]))?,
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
        let dependency = parse_dependency(&use_value, current_repository, raw_ids)?;
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
    raw_ids: &[String],
) -> Result<LiveDependency> {
    let (target, revision) = uses
        .rsplit_once('@')
        .ok_or_else(|| anyhow!("workflow dependency {uses} lacks immutable revision"))?;
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
        revision: revision.to_owned(),
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
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                byte as char
            } else {
                '-'
            }
        })
        .collect()
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
        let (reusable, actions, scanners) =
            parse_workflow_dependencies(yaml, "tailrocks/example", &["raw".to_owned()])
                .expect("dependencies");
        assert_eq!(reusable.len(), 1);
        assert_eq!(actions.len(), 1);
        assert_eq!(scanners.len(), 1);
        assert_eq!(reusable[0].revision, "abc123");
        assert!(parse_workflow_dependencies(
            "jobs: {build: {steps: [{uses: actions/checkout}]}}",
            "tailrocks/example",
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
            "head": {"repo": null, "ref": "feature", "sha": "a"},
            "base": {"repo": {"full_name": "tailrocks/example"}, "ref": "main", "sha": "b"},
            "merge_commit_sha": null,
            "html_url": "https://github.com/tailrocks/example/pull/7"
        });
        let identity =
            parse_pull_request_identity(&value, vec!["raw".to_owned()]).expect("identity");
        assert_eq!(identity.head_repository, None);
        assert_eq!(identity.tested_merge_sha, None);
    }
}
