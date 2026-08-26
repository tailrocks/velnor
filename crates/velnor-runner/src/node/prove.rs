//! Observed Ready preconditions. Controller never stamps these true.
//!
//! Routing is valid only when on-disk **evidence** equals **desired policy**
//! for group, selected repositories, labels, and trust scope. A boolean
//! `{valid: true, group_valid: true}` file, a URL-only file, or any empty
//! field is invalid (August 24 class: registration without repo access).

use std::path::{Path, PathBuf};
use std::process::Child;

use serde::{Deserialize, Serialize};

use crate::protocol::{
    github_json_request, github_json_request_with_rate_limit, GitHubScope, ListedWorkflowJob,
    RunnerGroup,
};

/// On-disk routing observation. Missing or invalid file is not valid routing.
pub const ROUTING_FILE: &str = "routing.json";
/// Desired routing policy. Reconciled independently of the scheduler backend.
pub const ROUTING_POLICY_FILE: &str = "routing-policy.json";
/// Observed GitHub group/repo/label/trust evidence.
pub const ROUTING_EVIDENCE_FILE: &str = "routing-evidence.json";
/// Host-local executor proof written by a real preflight, never by daemon startup.
pub const EXECUTOR_OK: &str = "executor.ok";
/// Transitional host Docker socket. Presence is not executor proof.
#[allow(dead_code)]
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutingFields {
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub selected_repositories: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub trust_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutingDocument {
    #[serde(default)]
    pub evidence: RoutingFields,
    #[serde(default)]
    pub policy: RoutingFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingObservation {
    pub valid: bool,
    pub group_valid: bool,
}

impl RoutingObservation {
    #[must_use]
    pub const fn invalid() -> Self {
        Self {
            valid: false,
            group_valid: false,
        }
    }
}

/// Read `routing.json`. Boolean stamps and URL-only files are invalid.
#[must_use]
pub fn observe_routing(state_dir: &Path) -> RoutingObservation {
    let Ok(bytes) = std::fs::read(state_dir.join(ROUTING_FILE)) else {
        return RoutingObservation::invalid();
    };
    let Ok(document) = serde_json::from_slice::<RoutingDocument>(&bytes) else {
        return RoutingObservation::invalid();
    };
    observe_document(&document)
}

#[must_use]
pub fn observe_document(document: &RoutingDocument) -> RoutingObservation {
    if !fields_complete(&document.evidence) || !fields_complete(&document.policy) {
        return RoutingObservation::invalid();
    }
    let group_valid = document.evidence.group == document.policy.group;
    let valid = group_valid && normalized(&document.evidence) == normalized(&document.policy);
    RoutingObservation { valid, group_valid }
}

/// Executor is proven only by a preflight `executor.ok` file.
/// A live Docker socket is the transitional backend, not the proof.
#[must_use]
pub fn observe_executor(state_dir: &Path) -> bool {
    state_dir.join(EXECUTOR_OK).is_file()
}

/// Session is live when the slot child is running or its journal pid still exists.
#[must_use]
pub fn observe_session(child: Option<&mut Child>, pid: Option<u32>) -> bool {
    if let Some(child) = child {
        if child.try_wait().ok().flatten().is_none() {
            return true;
        }
    }
    pid.is_some_and(pid_is_alive)
}

/// SIGNAL 0 existence check. Does not deliver a signal.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) only tests whether `pid` exists.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

/// Persist evidence and desired policy. Never a boolean Ready stamp.
///
/// # Errors
/// Directory or write failures.
pub fn write_routing_document(
    state_dir: &Path,
    evidence: RoutingFields,
    policy: RoutingFields,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(ROUTING_FILE);
    let body = serde_json::to_vec_pretty(&RoutingDocument { evidence, policy })?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Drift between desired policy and observed GitHub routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoutingDrift {
    pub missing_repositories: Vec<String>,
    pub extra_repositories: Vec<String>,
    pub group_mismatch: bool,
    pub labels_mismatch: bool,
    pub trust_mismatch: bool,
}

impl RoutingDrift {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.missing_repositories.is_empty()
            && self.extra_repositories.is_empty()
            && !self.group_mismatch
            && !self.labels_mismatch
            && !self.trust_mismatch
    }
}

/// Compare desired policy to observed GitHub routing. Independent of JIT vs scale-set.
#[must_use]
pub fn routing_drift(policy: &RoutingFields, evidence: &RoutingFields) -> RoutingDrift {
    let want = normalized(policy);
    let got = normalized(evidence);
    let missing_repositories = want
        .selected_repositories
        .iter()
        .filter(|repo| !got.selected_repositories.contains(repo))
        .cloned()
        .collect();
    let extra_repositories = got
        .selected_repositories
        .iter()
        .filter(|repo| !want.selected_repositories.contains(repo))
        .cloned()
        .collect();
    RoutingDrift {
        missing_repositories,
        extra_repositories,
        group_mismatch: want.group != got.group,
        labels_mismatch: want.labels != got.labels,
        trust_mismatch: want.trust_scope != got.trust_scope,
    }
}

const GITHUB_PROBE_TIMEOUT_SECS: u64 = 10;

/// Inputs for a live GitHub routing/reachability probe.
pub struct GitHubProbeRequest<'a> {
    pub url: &'a str,
    pub token: &'a str,
    pub policy: Option<&'a RoutingFields>,
    pub pool_id: Option<i64>,
    pub configured_labels: &'a [String],
    pub configured_trust: &'a str,
}

/// Live GitHub observation. Missing credentials are not reachable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitHubProbe {
    pub reachable: bool,
    pub evidence: Option<RoutingFields>,
    /// The shared PAT budget is exhausted; callers must pace to
    /// `rate_limit_reset_epoch` instead of retrying on the reconcile tick.
    pub rate_limited: bool,
    pub rate_limit_reset_epoch: Option<u64>,
    pub rate_limit_remaining: Option<u64>,
}

/// Trust scope the process is actually running with.
#[must_use]
pub fn runtime_trust_scope(configured: &str) -> String {
    std::env::var("VELNOR_TRUST_SCOPE").unwrap_or_else(|_| configured.to_owned())
}

/// Desired policy from a GitHub URL plus configured labels/trust.
#[must_use]
pub fn policy_from_github_url(
    url: &str,
    group: String,
    labels: Vec<String>,
    trust_scope: String,
) -> Option<RoutingFields> {
    let scope = GitHubScope::parse(url).ok()?;
    let selected_repositories = match scope.repo_full_name() {
        Some((owner, repo)) => vec![format!("{owner}/{repo}")],
        None => return None,
    };
    let fields = RoutingFields {
        group,
        selected_repositories,
        labels,
        trust_scope,
    };
    fields_complete(&fields).then_some(fields)
}

/// Directory of generated `<org>-desired-policy.json` files.
#[must_use]
pub fn generated_policy_dir() -> PathBuf {
    std::env::var_os("VELNOR_FLEET_POLICY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/velnor/fleet-policy"))
}

#[derive(Debug, Deserialize)]
struct GeneratedDesiredPolicy {
    organization: String,
    group_name: String,
    selected_repositories: Vec<String>,
}

/// Desired policy for org-scoped fleets: the generated allowlist, never live
/// group membership. Copying the first observed membership would bless a
/// truncated GitHub group (August 2026 drift class).
#[must_use]
pub fn org_policy_from_generated(
    org: &str,
    labels: Vec<String>,
    trust_scope: String,
    policy_dir: &Path,
) -> Option<RoutingFields> {
    let path = policy_dir.join(format!("{org}-desired-policy.json"));
    let generated: GeneratedDesiredPolicy =
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if generated.organization != org {
        return None;
    }
    let fields = RoutingFields {
        group: generated.group_name,
        selected_repositories: generated.selected_repositories,
        labels,
        trust_scope,
    };
    fields_complete(&fields).then_some(fields)
}

/// Read `routing-policy.json` when present.
#[must_use]
pub fn read_policy(state_dir: &Path) -> Option<RoutingFields> {
    read_fields(&state_dir.join(ROUTING_POLICY_FILE))
}

/// Write desired policy only when the operator has not already done so.
///
/// # Errors
/// Filesystem or JSON failures.
pub fn write_policy_if_absent(state_dir: &Path, policy: &RoutingFields) -> anyhow::Result<()> {
    let path = state_dir.join(ROUTING_POLICY_FILE);
    if path.is_file() {
        return Ok(());
    }
    write_fields(&path, policy)
}

/// Persist live GitHub routing evidence. Never a boolean Ready stamp.
///
/// # Errors
/// Filesystem or JSON failures.
pub fn write_evidence(state_dir: &Path, evidence: &RoutingFields) -> anyhow::Result<PathBuf> {
    let path = state_dir.join(ROUTING_EVIDENCE_FILE);
    write_fields(&path, evidence)?;
    Ok(path)
}

/// Probe GitHub reachability and compare live group/repos to desired policy.
/// Labels and trust are the process-configured values (what JIT will send).
/// Rate-limit telemetry from the response headers is surfaced so the
/// controller can pace the whole fleet to the token reset window.
pub async fn probe_github(request: GitHubProbeRequest<'_>) -> GitHubProbe {
    let Ok(scope) = GitHubScope::parse(request.url) else {
        return GitHubProbe::default();
    };
    let Ok(runners_url) = scope.runners_url() else {
        return GitHubProbe::default();
    };
    let Ok((status, body, rate_limit)) = github_json_request_with_rate_limit(
        "GET",
        runners_url.as_str(),
        request.token,
        None,
        GITHUB_PROBE_TIMEOUT_SECS,
    )
    .await
    else {
        return GitHubProbe::default();
    };
    if !(200..300).contains(&status) {
        if rate_limit.is_limited(status) {
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            return GitHubProbe {
                reachable: false,
                evidence: None,
                rate_limited: true,
                rate_limit_reset_epoch: rate_limit.reset_epoch_or_retry_after(now_epoch),
                rate_limit_remaining: rate_limit.remaining,
            };
        }
        return GitHubProbe::default();
    }
    let _ = body;
    let labels = if !request.configured_labels.is_empty() {
        request.configured_labels.to_vec()
    } else {
        request
            .policy
            .map(|policy| policy.labels.clone())
            .unwrap_or_default()
    };
    let trust_scope = runtime_trust_scope(request.configured_trust);
    let mut evidence = RoutingFields {
        group: request
            .policy
            .map(|policy| policy.group.clone())
            .unwrap_or_default(),
        selected_repositories: request
            .policy
            .map(|policy| policy.selected_repositories.clone())
            .unwrap_or_default(),
        labels,
        trust_scope,
    };
    if let Some((owner, repo)) = scope.repo_full_name() {
        evidence.selected_repositories = vec![format!("{owner}/{repo}")];
        if evidence.group.is_empty() {
            evidence.group = request
                .policy
                .map(|policy| policy.group.clone())
                .filter(|group| !group.is_empty())
                .unwrap_or_else(|| "Default".to_owned());
        }
    } else if let Some((group_name, repos)) = live_group_and_repos(
        &scope,
        request.token,
        request
            .policy
            .map(|policy| policy.group.as_str())
            .unwrap_or(""),
        request.pool_id,
    )
    .await
    {
        evidence.group = group_name;
        if !repos.is_empty() {
            evidence.selected_repositories = repos;
        }
    }
    GitHubProbe {
        reachable: true,
        evidence: Some(evidence),
        rate_limited: false,
        rate_limit_reset_epoch: rate_limit.rate_limit_reset_epoch,
        rate_limit_remaining: rate_limit.remaining,
    }
}

/// Queued, unassigned workflow jobs whose labels this fleet can serve.
pub async fn queued_job_ids(url: &str, token: &str, policy: &RoutingFields) -> Vec<String> {
    let Ok(scope) = GitHubScope::parse(url) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for repository in &policy.selected_repositories {
        ids.extend(queued_job_ids_for_repo(&scope, token, repository, policy).await);
    }
    ids
}

fn read_fields(path: &Path) -> Option<RoutingFields> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_fields(path: &Path, fields: &RoutingFields) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(fields)?)?;
    Ok(())
}

async fn live_group_and_repos(
    scope: &GitHubScope,
    token: &str,
    want_group: &str,
    pool_id: Option<i64>,
) -> Option<(String, Vec<String>)> {
    let url = scope.runner_groups_url().ok()?;
    let (status, body) =
        github_json_request("GET", url.as_str(), token, None, GITHUB_PROBE_TIMEOUT_SECS)
            .await
            .ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    #[derive(Deserialize)]
    struct Groups {
        runner_groups: Vec<RunnerGroup>,
    }
    let groups: Groups = serde_json::from_str(&body).ok()?;
    let group = groups
        .runner_groups
        .iter()
        .find(|group| pool_id == Some(group.id))
        .or_else(|| {
            groups
                .runner_groups
                .iter()
                .find(|group| !want_group.is_empty() && group.name == want_group)
        })
        .or_else(|| groups.runner_groups.iter().find(|group| !group.default))
        .or_else(|| groups.runner_groups.first())?
        .clone();
    let repos = live_group_repos(scope, token, group.id)
        .await
        .unwrap_or_default();
    Some((group.name, repos))
}

async fn live_group_repos(scope: &GitHubScope, token: &str, group_id: i64) -> Option<Vec<String>> {
    let url = scope.runner_group_repositories_url(group_id).ok()?;
    let (status, body) =
        github_json_request("GET", url.as_str(), token, None, GITHUB_PROBE_TIMEOUT_SECS)
            .await
            .ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    #[derive(Deserialize)]
    struct Page {
        repositories: Vec<Repo>,
    }
    #[derive(Deserialize)]
    struct Repo {
        full_name: String,
    }
    let page: Page = serde_json::from_str(&body).ok()?;
    Some(
        page.repositories
            .into_iter()
            .map(|repo| repo.full_name)
            .collect(),
    )
}

async fn queued_job_ids_for_repo(
    scope: &GitHubScope,
    token: &str,
    repository: &str,
    policy: &RoutingFields,
) -> Vec<String> {
    let mut url = match scope.repo_queued_runs_url(repository) {
        Ok(url) => url,
        Err(_) => return Vec::new(),
    };
    url.query_pairs_mut()
        .append_pair("status", "queued")
        .append_pair("per_page", "20");
    let Ok((status, body)) =
        github_json_request("GET", url.as_str(), token, None, GITHUB_PROBE_TIMEOUT_SECS).await
    else {
        return Vec::new();
    };
    if !(200..300).contains(&status) {
        return Vec::new();
    }
    #[derive(Deserialize)]
    struct Runs {
        workflow_runs: Vec<Run>,
    }
    #[derive(Deserialize)]
    struct Run {
        id: u64,
    }
    let Ok(runs) = serde_json::from_str::<Runs>(&body) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for run in runs.workflow_runs {
        ids.extend(queued_jobs_for_run(scope, token, repository, run.id, policy).await);
    }
    ids
}

async fn queued_jobs_for_run(
    scope: &GitHubScope,
    token: &str,
    repository: &str,
    run_id: u64,
    policy: &RoutingFields,
) -> Vec<String> {
    let Ok(url) = scope
        .api_base_url
        .join(&format!("repos/{repository}/actions/runs/{run_id}/jobs"))
    else {
        return Vec::new();
    };
    let Ok((status, body)) =
        github_json_request("GET", url.as_str(), token, None, GITHUB_PROBE_TIMEOUT_SECS).await
    else {
        return Vec::new();
    };
    if !(200..300).contains(&status) {
        return Vec::new();
    }
    #[derive(Deserialize)]
    struct Jobs {
        jobs: Vec<ListedWorkflowJob>,
    }
    let Ok(jobs) = serde_json::from_str::<Jobs>(&body) else {
        return Vec::new();
    };
    jobs.jobs
        .into_iter()
        .filter(|job| {
            job.runner_id.is_none()
                && job.status.as_deref() == Some("queued")
                && job
                    .labels
                    .iter()
                    .all(|label| policy.labels.iter().any(|have| have == label))
        })
        .map(|job| job.id.to_string())
        .collect()
}

/// Write `routing.json` from policy + evidence files when both exist.
///
/// # Errors
/// Filesystem or JSON failures.
pub fn reconcile_from_dir(state_dir: &Path) -> anyhow::Result<Option<RoutingObservation>> {
    let policy_path = state_dir.join(ROUTING_POLICY_FILE);
    let evidence_path = state_dir.join(ROUTING_EVIDENCE_FILE);
    if !policy_path.is_file() || !evidence_path.is_file() {
        return Ok(None);
    }
    let policy: RoutingFields = serde_json::from_slice(&std::fs::read(policy_path)?)?;
    let evidence: RoutingFields = serde_json::from_slice(&std::fs::read(evidence_path)?)?;
    write_routing_document(state_dir, evidence.clone(), policy.clone())?;
    Ok(Some(observe_document(&RoutingDocument {
        evidence,
        policy,
    })))
}

/// Record a preflight executor proof file. Daemon startup must not call this.
///
/// # Errors
/// Directory or write failures.
pub fn write_executor_ok(state_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(EXECUTOR_OK);
    std::fs::write(&path, b"ok\n")?;
    Ok(path)
}

fn fields_complete(fields: &RoutingFields) -> bool {
    !fields.group.is_empty()
        && !fields.selected_repositories.is_empty()
        && fields
            .selected_repositories
            .iter()
            .all(|repo| !repo.is_empty())
        && !fields.labels.is_empty()
        && fields.labels.iter().all(|label| !label.is_empty())
        && !fields.trust_scope.is_empty()
}

fn normalized(fields: &RoutingFields) -> RoutingFields {
    let mut selected_repositories = fields.selected_repositories.clone();
    selected_repositories.sort();
    let mut labels = fields.labels.clone();
    labels.sort();
    RoutingFields {
        group: fields.group.clone(),
        selected_repositories,
        labels,
        trust_scope: fields.trust_scope.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-prove-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn matching_fields() -> RoutingFields {
        RoutingFields {
            group: "velnor".into(),
            selected_repositories: vec!["tailrocks/velnor".into()],
            labels: vec!["velnor".into()],
            trust_scope: "trusted".into(),
        }
    }

    #[test]
    fn missing_routing_file_is_invalid() {
        let dir = tmp("missing");
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn boolean_stamp_is_invalid() {
        let dir = tmp("bool");
        std::fs::write(
            dir.join(ROUTING_FILE),
            br#"{"valid":true,"group_valid":true}"#,
        )
        .unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn url_only_json_is_invalid() {
        let dir = tmp("url");
        std::fs::write(
            dir.join(ROUTING_FILE),
            br#"{"url":"https://github.com/o/r"}"#,
        )
        .unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn matching_evidence_and_policy_is_valid() {
        let dir = tmp("match");
        let fields = matching_fields();
        write_routing_document(&dir, fields.clone(), fields).unwrap();
        assert_eq!(
            observe_routing(&dir),
            RoutingObservation {
                valid: true,
                group_valid: true
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn repo_mismatch_is_invalid() {
        let dir = tmp("mismatch");
        let evidence = matching_fields();
        let mut policy = matching_fields();
        policy.selected_repositories = vec!["other/repo".into()];
        write_routing_document(&dir, evidence, policy).unwrap();
        let observed = observe_routing(&dir);
        assert!(!observed.valid);
        assert!(observed.group_valid);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn executor_ok_file_is_proof() {
        let dir = tmp("exec");
        assert!(!observe_executor(&dir));
        write_executor_ok(&dir).unwrap();
        assert!(observe_executor(&dir));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn routing_drift_is_independent_of_scheduler() {
        let policy = matching_fields();
        let mut evidence = matching_fields();
        evidence.selected_repositories = vec!["other/repo".into()];
        let drift = routing_drift(&policy, &evidence);
        assert!(!drift.is_empty());
        assert_eq!(drift.missing_repositories, vec!["tailrocks/velnor"]);
        assert_eq!(drift.extra_repositories, vec!["other/repo"]);
        assert!(!drift.group_mismatch);
    }

    #[test]
    fn reconcile_from_dir_writes_observation() {
        let dir = tmp("reconcile");
        let policy = matching_fields();
        std::fs::write(
            dir.join(ROUTING_POLICY_FILE),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join(ROUTING_EVIDENCE_FILE),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let observed = reconcile_from_dir(&dir).unwrap().unwrap();
        assert!(observed.valid);
        assert!(observed.group_valid);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_labels_are_invalid() {
        let dir = tmp("labels");
        let mut fields = matching_fields();
        fields.labels.clear();
        write_routing_document(&dir, fields.clone(), fields).unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn policy_from_repo_url_is_complete() {
        let policy = policy_from_github_url(
            "https://github.com/tailrocks/velnor",
            "velnor".into(),
            vec!["velnor".into()],
            "trusted".into(),
        )
        .unwrap();
        assert_eq!(policy.selected_repositories, vec!["tailrocks/velnor"]);
        assert!(fields_complete(&policy));
    }

    #[test]
    fn policy_from_org_url_is_not_inferred() {
        assert!(policy_from_github_url(
            "https://github.com/tailrocks",
            "velnor".into(),
            vec!["velnor".into()],
            "trusted".into(),
        )
        .is_none());
    }

    #[test]
    fn org_policy_comes_from_generated_allowlist_not_live_membership() {
        let dir = tmp("generated-policy");
        let generated = serde_json::json!({
            "organization": "tailrocks",
            "group_name": "velnor-trusted",
            "selected_repositories": ["tailrocks/velnor", "tailrocks/velnor-apt"]
        });
        std::fs::write(
            dir.join("tailrocks-desired-policy.json"),
            serde_json::to_vec(&generated).unwrap(),
        )
        .unwrap();
        let policy =
            org_policy_from_generated("tailrocks", vec!["velnor".into()], "trusted".into(), &dir)
                .unwrap();
        assert_eq!(
            policy.selected_repositories,
            vec!["tailrocks/velnor", "tailrocks/velnor-apt"]
        );
        assert_eq!(policy.group, "velnor-trusted");
        assert!(org_policy_from_generated(
            "tailrocks",
            vec!["velnor".into()],
            "trusted".into(),
            &tmp("missing-policy"),
        )
        .is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn org_probe_without_policy_writes_live_group_and_repos() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0,
                "runners": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "runner_groups": [{
                    "id": 7,
                    "name": "velnor",
                    "default": false
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "repositories": [{"full_name": "tailrocks/velnor"}]
            })))
            .mount(&server)
            .await;

        let url = format!("{}/tailrocks", server.uri());
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: None,
            pool_id: None,
            configured_labels: &["velnor".into()],
            configured_trust: "trusted",
        })
        .await;
        assert!(probe.reachable, "url={url} probe={probe:?}");
        let evidence = probe.evidence.expect("live evidence");
        assert_eq!(evidence.group, "velnor");
        assert_eq!(evidence.selected_repositories, vec!["tailrocks/velnor"]);
        assert_eq!(evidence.labels, vec!["velnor"]);
        assert_eq!(evidence.trust_scope, "trusted");

        let dir = tmp("org-probe");
        write_evidence(&dir, &evidence).unwrap();
        let on_disk: RoutingFields =
            serde_json::from_slice(&std::fs::read(dir.join(ROUTING_EVIDENCE_FILE)).unwrap())
                .unwrap();
        assert_eq!(on_disk, evidence);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Secondary-limit 403 carrying `Retry-After` but no
    /// `x-ratelimit-reset`: the probe must surface a deadline derived from
    /// `Retry-After` so the controller paces to the requested delay instead
    /// of the fixed 600s fallback (end-to-end over the native transport).
    #[tokio::test]
    async fn rate_limited_probe_derives_reset_from_retry_after() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("retry-after", "30")
                    .insert_header("x-ratelimit-remaining", "4999"),
            )
            .mount(&server)
            .await;

        let url = format!("{}/tailrocks", server.uri());
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: None,
            pool_id: None,
            configured_labels: &["velnor".into()],
            configured_trust: "trusted",
        })
        .await;
        assert!(!probe.reachable);
        assert!(probe.rate_limited, "probe={probe:?}");
        let reset = probe
            .rate_limit_reset_epoch
            .expect("retry-after must become a reset deadline");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            reset >= now + 30 && reset <= now + 31,
            "reset={reset} now={now}"
        );
    }
}
