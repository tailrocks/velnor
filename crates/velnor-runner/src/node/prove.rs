//! Observed Ready preconditions. Controller never stamps these true.
//!
//! Routing is valid only when on-disk **evidence** equals **desired policy**
//! for group, selected repositories, labels, and trust scope. A boolean
//! `{valid: true, group_valid: true}` file, a URL-only file, or any empty
//! field is invalid (August 24 class: registration without repo access).

use anyhow::{bail, Context, Result};
use std::process::Child;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use velnor_model::{ExecutionBackendKind, Generation, SlotId};

use crate::protocol::{
    github_json_request_with_rate_limit, GitHubRateLimitStatus, GitHubScope, ListedWorkflowJob,
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
/// Host Docker socket. Executor proof for the docker backend only.
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

/// Executor proof is backend-specific. MicroVM never treats the host Docker
/// socket as ready.
#[must_use]
pub fn observe_executor(state_dir: &Path, backend: ExecutionBackendKind) -> bool {
    crate::execution::executor_is_proven(state_dir, backend, Path::new(HOST_DOCKER_SOCKET))
}

/// Legacy process-liveness helper. Slot proofs must use
/// [`observe_slot_session`], which requires fresh generation-bound evidence.
#[must_use]
pub fn observe_session(child: Option<&mut Child>, pid: Option<u32>) -> bool {
    if let Some(child) = child
        && child.try_wait().ok().flatten().is_none()
    {
        return true;
    }
    pid.is_some_and(pid_is_alive)
}

/// Session proof for a controller-recovered slot.
///
/// `child` and `pid` remain in the public signature for compatibility, but
/// process liveness alone is not evidence: only a fresh heartbeat with the
/// expected generation and command-line identity can prove the slot session.
#[must_use]
pub fn observe_slot_session(
    child: Option<&mut Child>,
    pid: Option<u32>,
    state_dir: &std::path::Path,
    slot_id: &SlotId,
    generation: Generation,
) -> bool {
    let _ = (child, pid);
    slot_heartbeat_is_fresh(state_dir, slot_id, generation, Duration::from_secs(10))
}

/// SIGNAL 0 existence check. Does not deliver a signal.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) only tests whether `pid` exists.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

/// Verify that a persisted PID is the slot actor Velnor launched, not merely
/// an unrelated process that reused the number after a controller restart.
/// Linux exposes the child argv through procfs. Other Unix targets use the
/// standard `ps` command and reject any missing, malformed, or mismatched
/// command-line evidence.
#[must_use]
pub fn slot_process_is_alive(
    pid: u32,
    state_dir: &std::path::Path,
    slot_id: &SlotId,
    generation: Generation,
) -> bool {
    if !pid_is_alive(pid) {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        let Some((scope, index)) = slot_id.0.rsplit_once('-') else {
            return false;
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(_) => return false,
        };
        let args: Vec<&[u8]> = cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .collect();
        let Some(executable) = args
            .first()
            .and_then(|arg| arg.rsplit(|byte| *byte == b'/').next())
        else {
            return false;
        };
        if executable != b"velnor-runner" {
            return false;
        }
        let state_dir = state_dir.to_string_lossy();
        let generation = generation.0.to_string();
        let expected = [
            b"slot".as_slice(),
            b"--state-dir".as_slice(),
            state_dir.as_bytes(),
            b"--scope".as_slice(),
            scope.as_bytes(),
            b"--slot-index".as_slice(),
            index.as_bytes(),
            b"--generation".as_slice(),
            generation.as_bytes(),
        ];
        args.get(1..)
            .is_some_and(|args| args == expected.as_slice())
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Some((scope, index)) = slot_id.0.rsplit_once('-') else {
            return false;
        };
        let Some(state_dir) = state_dir.to_str() else {
            return false;
        };
        let generation = generation.0.to_string();
        let pid = pid.to_string();
        let Ok(output) = std::process::Command::new("ps")
            .args(["-ww", "-p", &pid, "-o", "command="])
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let Ok(command_line) = std::str::from_utf8(&output.stdout) else {
            return false;
        };
        let mut args = command_line.split_ascii_whitespace();
        let Some(executable) = args.next() else {
            return false;
        };
        if std::path::Path::new(executable)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            != Some("velnor-runner")
        {
            return false;
        }
        let expected = [
            "slot",
            "--state-dir",
            state_dir,
            "--scope",
            scope,
            "--slot-index",
            index,
            "--generation",
            generation.as_str(),
        ];
        let args: Vec<&str> = args.collect();
        args.as_slice() == expected.as_slice()
    }

    #[cfg(not(unix))]
    {
        let _ = (state_dir, slot_id, generation);
        pid_is_alive(pid)
    }
}

/// Verify that a slot published recent progress for the expected generation
/// and that its recorded process is still the expected live actor.
#[must_use]
pub fn slot_heartbeat_is_fresh(
    state_dir: &Path,
    slot_id: &SlotId,
    generation: Generation,
    max_age: Duration,
) -> bool {
    let Some((scope, index)) = slot_id.0.rsplit_once('-') else {
        return false;
    };
    if scope.is_empty() {
        return false;
    }
    let Ok(index) = index.parse::<usize>() else {
        return false;
    };
    let path = super::slot::heartbeat_path(state_dir, index);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(age) = metadata
        .modified()
        .and_then(|time| time.elapsed().map_err(std::io::Error::other))
    else {
        return false;
    };
    if max_age.is_zero() || age > max_age {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(heartbeat) = serde_json::from_slice::<super::slot::SlotHeartbeat>(&bytes) else {
        return false;
    };
    heartbeat.generation == generation.0
        && heartbeat.sequence > 0
        && slot_process_is_alive(heartbeat.pid, state_dir, slot_id, generation)
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
const GITHUB_PAGE_SIZE: usize = 100;
const GITHUB_MAX_PAGES: u32 = 100;
const GITHUB_MAX_ITEMS: usize = GITHUB_PAGE_SIZE * GITHUB_MAX_PAGES as usize;

/// Preserve the most restrictive rate-limit telemetry across all requests in
/// one probe. The controller paces on the lowest remaining budget and latest
/// reset deadline, so a later response cannot erase an earlier exhaustion
/// signal by omitting headers or reporting a healthier window.
fn aggregate_rate_limit_status(
    current: GitHubRateLimitStatus,
    next: GitHubRateLimitStatus,
) -> GitHubRateLimitStatus {
    GitHubRateLimitStatus {
        retry_after_seconds: current
            .retry_after_seconds
            .into_iter()
            .chain(next.retry_after_seconds)
            .max(),
        rate_limit_reset_epoch: current
            .rate_limit_reset_epoch
            .into_iter()
            .chain(next.rate_limit_reset_epoch)
            .max(),
        remaining: current.remaining.into_iter().chain(next.remaining).min(),
    }
}

/// Inputs for a live GitHub routing/reachability probe.
pub struct GitHubProbeRequest<'a> {
    pub url: &'a str,
    pub token: &'a str,
    pub policy: Option<&'a RoutingFields>,
    pub configured_group: Option<&'a str>,
    pub pool_id: Option<i64>,
    pub configured_labels: &'a [String],
    pub configured_trust: &'a str,
}

/// Live GitHub observation. Missing credentials are not reachable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitHubProbe {
    pub reachable: bool,
    pub evidence: Option<RoutingFields>,
    /// Explicit reason the probe failed closed, when reachability was not
    /// enough to establish the configured routing identity.
    pub diagnostic: Option<String>,
    /// The shared PAT budget is exhausted; callers must pace to
    /// `rate_limit_reset_epoch` instead of retrying on the reconcile tick.
    pub rate_limited: bool,
    pub rate_limit_reset_epoch: Option<u64>,
    pub rate_limit_remaining: Option<u64>,
}

impl GitHubProbe {
    fn failed(diagnostic: impl Into<String>) -> Self {
        Self {
            diagnostic: Some(diagnostic.into()),
            ..Self::default()
        }
    }
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

/// Read an operator-declared desired policy from an explicit path. Live
/// GitHub evidence is never promoted to policy by this function.
pub fn read_policy_file(path: &Path) -> anyhow::Result<RoutingFields> {
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("read routing policy {}: {error}", path.display()))?;
    let fields: RoutingFields = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("parse routing policy {}: {error}", path.display()))?;
    if !fields_complete(&fields) {
        anyhow::bail!(
            "routing policy {} is incomplete: group, selected_repositories, labels, and trust_scope are required",
            path.display()
        );
    }
    Ok(fields)
}

/// Write desired policy only when the operator has not already done so.
/// Repo-scoped fleets use this so an operator override wins. Org fleets
/// must call [`write_policy`]: a live-membership snapshot must not remain
/// the desired baseline.
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

/// Replace a policy derived from immutable daemon configuration only when it
/// changed. Explicit `routing_policy_file` overrides use `write_policy_file`
/// and never pass through this helper.
pub fn write_policy_if_changed(state_dir: &Path, policy: &RoutingFields) -> anyhow::Result<()> {
    let path = state_dir.join(ROUTING_POLICY_FILE);
    if read_fields(&path).as_ref() == Some(policy) {
        return Ok(());
    }
    write_fields(&path, policy)
}

/// Replace desired policy. Org fleets rewrite this from the generated
/// allowlist every cycle so a stale live-membership snapshot cannot stay
/// the desired baseline.
///
/// # Errors
/// Filesystem or JSON failures.
pub fn write_policy(state_dir: &Path, policy: &RoutingFields) -> anyhow::Result<()> {
    write_fields(&state_dir.join(ROUTING_POLICY_FILE), policy)
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
    let scope = match GitHubScope::parse(request.url) {
        Ok(scope) => scope,
        Err(error) => return GitHubProbe::failed(format!("parse GitHub URL: {error}")),
    };
    let runners_url = match scope.runners_url() {
        Ok(url) => url,
        Err(error) => {
            return GitHubProbe::failed(format!("build runners URL: {error}"));
        }
    };
    let (status, body, rate_limit) = match github_json_request_with_rate_limit(
        "GET",
        runners_url.as_str(),
        request.token,
        None,
        GITHUB_PROBE_TIMEOUT_SECS,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return GitHubProbe::failed(format!("request runners: {error}")),
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
                diagnostic: Some(format!(
                    "GitHub routing probe rate-limited; routing identity is unproven (HTTP {status})"
                )),
                rate_limited: true,
                rate_limit_reset_epoch: rate_limit.reset_epoch_or_retry_after(now_epoch),
                rate_limit_remaining: rate_limit.remaining,
            };
        }
        return GitHubProbe::failed(format!("runners request returned HTTP {status}"));
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
    let mut final_rate_limit = rate_limit;
    if let Some((owner, repo)) = scope.repo_full_name() {
        evidence.selected_repositories = vec![format!("{owner}/{repo}")];
        if let Some(group) = repository_group_identity(request.configured_group) {
            evidence.group = group;
        } else {
            return GitHubProbe {
                reachable: true,
                evidence: None,
                diagnostic: Some(
                    "routing identity unproven: configured runner group name or id is missing"
                        .to_owned(),
                ),
                rate_limited: false,
                rate_limit_reset_epoch: final_rate_limit.rate_limit_reset_epoch,
                rate_limit_remaining: final_rate_limit.remaining,
            };
        }
    } else {
        let live = live_group_and_repos(
            &scope,
            request.token,
            request
                .policy
                .map(|policy| policy.group.as_str())
                .unwrap_or(""),
            request.pool_id,
        )
        .await;
        let (group_name, repos, live_rate_limit) = match live {
            Ok(live) => live,
            Err(error) => {
                let rate_limit = aggregate_rate_limit_status(final_rate_limit, error.rate_limit);
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                return GitHubProbe {
                    reachable: !error.rate_limited,
                    evidence: None,
                    diagnostic: Some(format!("routing identity unproven: {}", error.message)),
                    rate_limited: error.rate_limited,
                    rate_limit_reset_epoch: rate_limit.reset_epoch_or_retry_after(now_epoch),
                    rate_limit_remaining: rate_limit.remaining,
                };
            }
        };
        final_rate_limit = aggregate_rate_limit_status(final_rate_limit, live_rate_limit);
        evidence.group = group_name;
        if repos.is_empty() {
            return GitHubProbe {
                reachable: true,
                evidence: None,
                diagnostic: Some(
                    "routing identity unproven: GitHub returned zero runner-group repositories"
                        .to_owned(),
                ),
                rate_limited: false,
                rate_limit_reset_epoch: final_rate_limit.rate_limit_reset_epoch,
                rate_limit_remaining: final_rate_limit.remaining,
            };
        }
        evidence.selected_repositories = repos;
    }
    GitHubProbe {
        reachable: true,
        evidence: Some(evidence),
        diagnostic: None,
        rate_limited: false,
        rate_limit_reset_epoch: final_rate_limit.rate_limit_reset_epoch,
        rate_limit_remaining: final_rate_limit.remaining,
    }
}

fn repository_group_identity(configured_group: Option<&str>) -> Option<String> {
    configured_group
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
}

/// Queued, unassigned workflow jobs whose labels this fleet can serve.
pub async fn queued_job_ids(url: &str, token: &str, policy: &RoutingFields) -> Result<Vec<String>> {
    let scope = GitHubScope::parse(url).context("parse GitHub URL for queued-job probe")?;
    let mut ids = Vec::new();
    for repository in &policy.selected_repositories {
        ids.extend(
            queued_job_ids_for_repo(&scope, token, repository, policy)
                .await
                .with_context(|| format!("probe queued jobs for {repository}"))?,
        );
    }
    Ok(ids)
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
) -> Result<(String, Vec<String>, GitHubRateLimitStatus), LiveLookupError> {
    if want_group.is_empty() && pool_id.is_none() {
        return Err(LiveLookupError::message(
            "configured runner group identity is missing",
        ));
    }
    let base_url = scope
        .runner_groups_url()
        .map_err(|error| LiveLookupError::message(format!("build runner-groups URL: {error}")))?;
    #[derive(Deserialize)]
    struct Groups {
        total_count: u64,
        runner_groups: Vec<RunnerGroup>,
    }
    let (group, rate_limit) = if let Some(pool_id) = pool_id {
        let url = scope.runner_group_url(pool_id).map_err(|error| {
            LiveLookupError::message(format!("build runner-group URL: {error}"))
        })?;
        let (status, body, rate_limit) = github_json_request_with_rate_limit(
            "GET",
            url.as_str(),
            token,
            None,
            GITHUB_PROBE_TIMEOUT_SECS,
        )
        .await
        .map_err(|error| LiveLookupError::message(format!("request runner group: {error}")))?;
        if !(200..300).contains(&status) {
            return Err(LiveLookupError::response(
                "runner-group",
                status,
                body,
                rate_limit,
            ));
        }
        let group: RunnerGroup = serde_json::from_str(&body).map_err(|error| {
            LiveLookupError::with_rate_limit(
                format!("parse runner-group response: {error}"),
                rate_limit,
            )
        })?;
        let group = resolve_live_group(std::slice::from_ref(&group), want_group, Some(pool_id))
            .map_err(|error| LiveLookupError::with_rate_limit(error, rate_limit))?;
        (group, rate_limit)
    } else {
        let mut page_number = 1u32;
        let mut items_seen = 0usize;
        let mut rate_limit = GitHubRateLimitStatus::default();
        let mut expected_total = None;
        let mut group_ids = BTreeSet::new();
        let mut group_names = BTreeSet::new();
        let group = loop {
            if page_number > GITHUB_MAX_PAGES || items_seen >= GITHUB_MAX_ITEMS {
                return Err(LiveLookupError::with_rate_limit(
                format!(
                    "runner-group lookup exceeded bounded response limit ({GITHUB_MAX_ITEMS} groups)"
                ),
                rate_limit,
            ));
            }
            let mut url = base_url.clone();
            url.query_pairs_mut()
                .append_pair("per_page", &GITHUB_PAGE_SIZE.to_string())
                .append_pair("page", &page_number.to_string());
            let (status, body, page_rate_limit) = github_json_request_with_rate_limit(
                "GET",
                url.as_str(),
                token,
                None,
                GITHUB_PROBE_TIMEOUT_SECS,
            )
            .await
            .map_err(|error| LiveLookupError::message(format!("request runner groups: {error}")))?;
            rate_limit = aggregate_rate_limit_status(rate_limit, page_rate_limit);
            if !(200..300).contains(&status) {
                return Err(LiveLookupError::response(
                    "runner-groups",
                    status,
                    body,
                    rate_limit,
                ));
            }
            let page: Groups = serde_json::from_str(&body).map_err(|error| {
                LiveLookupError::with_rate_limit(
                    format!("parse runner-groups response: {error}"),
                    rate_limit,
                )
            })?;
            let fetched = page.runner_groups.len();
            items_seen = items_seen.saturating_add(fetched);
            if page.total_count > GITHUB_MAX_ITEMS as u64 || items_seen as u64 > page.total_count {
                return Err(LiveLookupError::with_rate_limit(
                    format!(
                        "runner-group response has inconsistent total_count {} after {} items",
                        page.total_count, items_seen
                    ),
                    rate_limit,
                ));
            }
            if expected_total.is_some_and(|total| total != page.total_count) {
                return Err(LiveLookupError::with_rate_limit(
                    "runner-group total_count changed during pagination",
                    rate_limit,
                ));
            }
            expected_total = Some(page.total_count);
            for group in &page.runner_groups {
                if !group_ids.insert(group.id) || !group_names.insert(group.name.clone()) {
                    return Err(LiveLookupError::with_rate_limit(
                        "runner-group pagination returned duplicate identities",
                        rate_limit,
                    ));
                }
            }
            if let Some(group) = page.runner_groups.iter().find(|group| {
                pool_id.is_some_and(|id| group.id == id)
                    || (!want_group.is_empty() && group.name == want_group)
            }) {
                break resolve_live_group(std::slice::from_ref(group), want_group, pool_id)
                    .map_err(|error| LiveLookupError::with_rate_limit(error, rate_limit))?;
            }
            if fetched < GITHUB_PAGE_SIZE || items_seen as u64 >= page.total_count {
                if items_seen as u64 != page.total_count {
                    return Err(LiveLookupError::with_rate_limit(
                        format!(
                            "runner-group pagination ended at {} of {} items",
                            items_seen, page.total_count
                        ),
                        rate_limit,
                    ));
                }
                let identity = pool_id
                    .map(|id| format!("id {id}"))
                    .unwrap_or_else(|| format!("name '{want_group}'"));
                return Err(LiveLookupError::with_rate_limit(
                    format!("configured runner group {identity} was not found"),
                    rate_limit,
                ));
            }
            page_number += 1;
        };
        (group, rate_limit)
    };
    let (repos, repos_rate_limit) = match live_group_repos(scope, token, group.id).await {
        Ok(result) => result,
        Err(mut error) => {
            error.rate_limit = aggregate_rate_limit_status(rate_limit, error.rate_limit);
            return Err(error);
        }
    };
    Ok((
        group.name,
        repos,
        aggregate_rate_limit_status(rate_limit, repos_rate_limit),
    ))
}

#[derive(Debug)]
struct LiveLookupError {
    message: String,
    rate_limit: GitHubRateLimitStatus,
    rate_limited: bool,
}

impl LiveLookupError {
    fn message(message: impl Into<String>) -> Self {
        Self::with_rate_limit(message, GitHubRateLimitStatus::default())
    }

    fn with_rate_limit(message: impl Into<String>, rate_limit: GitHubRateLimitStatus) -> Self {
        Self {
            message: message.into(),
            rate_limited: false,
            rate_limit,
        }
    }

    fn response(
        endpoint: &str,
        status: u16,
        body: String,
        rate_limit: GitHubRateLimitStatus,
    ) -> Self {
        Self {
            message: format!("{endpoint} request returned HTTP {status}: {}", body.trim()),
            rate_limited: rate_limit.is_limited(status),
            rate_limit,
        }
    }
}

fn resolve_live_group(
    groups: &[RunnerGroup],
    requested_name: &str,
    requested_id: Option<i64>,
) -> Result<RunnerGroup, String> {
    if groups.is_empty() {
        return Err(format!(
            "runner group '{requested_name}' is unavailable: GitHub returned zero runner groups"
        ));
    }

    let group = if let Some(requested_id) = requested_id {
        let group = groups
            .iter()
            .find(|group| group.id == requested_id)
            .ok_or_else(|| {
                format!(
                "configured runner group '{requested_name}' with id {requested_id} was not found"
            )
            })?;
        if !requested_name.is_empty() && group.name != requested_name {
            return Err(format!(
                "configured runner group '{requested_name}' resolves to id {}, not configured id {requested_id}",
                group.id
            ));
        }
        group
    } else if !requested_name.is_empty() {
        groups
            .iter()
            .find(|group| group.name == requested_name)
            .ok_or_else(|| format!("configured runner group '{requested_name}' was not found"))?
    } else {
        return Err("configured runner group identity is missing".to_owned());
    };
    Ok(group.clone())
}

async fn live_group_repos(
    scope: &GitHubScope,
    token: &str,
    group_id: i64,
) -> Result<(Vec<String>, GitHubRateLimitStatus), LiveLookupError> {
    let base_url = scope
        .runner_group_repositories_url(group_id)
        .map_err(|error| {
            LiveLookupError::message(format!("build runner-group repositories URL: {error}"))
        })?;
    #[derive(Deserialize)]
    struct Page {
        total_count: u64,
        repositories: Vec<Repo>,
    }
    #[derive(Deserialize)]
    struct Repo {
        full_name: String,
    }
    let mut repositories = Vec::new();
    let mut page_number = 1u32;
    let mut items_seen = 0usize;
    let mut rate_limit = GitHubRateLimitStatus::default();
    let mut expected_total = None;
    let mut repository_names = BTreeSet::new();
    loop {
        if page_number > GITHUB_MAX_PAGES || items_seen >= GITHUB_MAX_ITEMS {
            return Err(LiveLookupError::with_rate_limit(
                format!(
                    "runner-group repository lookup exceeded bounded response limit ({GITHUB_MAX_ITEMS} repositories)"
                ),
                rate_limit,
            ));
        }
        let mut url = base_url.clone();
        url.query_pairs_mut()
            .append_pair("per_page", &GITHUB_PAGE_SIZE.to_string())
            .append_pair("page", &page_number.to_string());
        let (status, body, page_rate_limit) = github_json_request_with_rate_limit(
            "GET",
            url.as_str(),
            token,
            None,
            GITHUB_PROBE_TIMEOUT_SECS,
        )
        .await
        .map_err(|error| {
            LiveLookupError::message(format!("request runner-group repositories: {error}"))
        })?;
        rate_limit = aggregate_rate_limit_status(rate_limit, page_rate_limit);
        if !(200..300).contains(&status) {
            return Err(LiveLookupError::response(
                "runner-group repositories",
                status,
                body,
                rate_limit,
            ));
        }
        let page: Page = serde_json::from_str(&body).map_err(|error| {
            LiveLookupError::with_rate_limit(
                format!("parse runner-group repositories response: {error}"),
                rate_limit,
            )
        })?;
        if page.total_count > GITHUB_MAX_ITEMS as u64 {
            return Err(LiveLookupError::with_rate_limit(
                format!(
                    "runner-group repository response exceeds bounded response limit ({})",
                    GITHUB_MAX_ITEMS
                ),
                rate_limit,
            ));
        }
        if expected_total.is_some_and(|total| total != page.total_count) {
            return Err(LiveLookupError::with_rate_limit(
                "runner-group repository total_count changed during pagination",
                rate_limit,
            ));
        }
        expected_total = Some(page.total_count);
        let fetched = page.repositories.len();
        items_seen = items_seen.saturating_add(fetched);
        if items_seen as u64 > page.total_count {
            return Err(LiveLookupError::with_rate_limit(
                "runner-group repository pagination returned more items than total_count",
                rate_limit,
            ));
        }
        for repo in page.repositories {
            if !repository_names.insert(repo.full_name.clone()) {
                return Err(LiveLookupError::with_rate_limit(
                    "runner-group repository pagination returned duplicate identities",
                    rate_limit,
                ));
            }
            repositories.push(repo.full_name);
        }
        if fetched < GITHUB_PAGE_SIZE || items_seen as u64 >= page.total_count {
            if items_seen as u64 != page.total_count {
                return Err(LiveLookupError::with_rate_limit(
                    format!(
                        "runner-group repository pagination ended at {} of {} items",
                        items_seen, page.total_count
                    ),
                    rate_limit,
                ));
            }
            repositories.sort_unstable();
            return Ok((repositories, rate_limit));
        }
        page_number += 1;
    }
}

async fn queued_job_ids_for_repo(
    scope: &GitHubScope,
    token: &str,
    repository: &str,
    policy: &RoutingFields,
) -> Result<Vec<String>> {
    let mut url = scope
        .repo_queued_runs_url(repository)
        .with_context(|| format!("build queued workflow runs URL for {repository}"))?;
    url.query_pairs_mut()
        .append_pair("status", "queued")
        .append_pair("per_page", "20");
    let (status, body, _rate_limit) = github_json_request_with_rate_limit(
        "GET",
        url.as_str(),
        token,
        None,
        GITHUB_PROBE_TIMEOUT_SECS,
    )
    .await
    .context("request queued workflow runs")?;
    if !(200..300).contains(&status) {
        bail!(
            "queued workflow runs request returned HTTP {status}: {}",
            body.trim()
        );
    }
    #[derive(Deserialize)]
    struct Runs {
        workflow_runs: Vec<Run>,
    }
    #[derive(Deserialize)]
    struct Run {
        id: u64,
    }
    let runs: Runs = serde_json::from_str(&body).context("parse queued workflow runs response")?;
    let mut ids = Vec::new();
    for run in runs.workflow_runs {
        ids.extend(
            queued_jobs_for_run(scope, token, repository, run.id, policy)
                .await
                .with_context(|| format!("probe queued jobs for {repository} run {}", run.id))?,
        );
    }
    Ok(ids)
}

async fn queued_jobs_for_run(
    scope: &GitHubScope,
    token: &str,
    repository: &str,
    run_id: u64,
    policy: &RoutingFields,
) -> Result<Vec<String>> {
    let url = scope
        .api_base_url
        .join(&format!("repos/{repository}/actions/runs/{run_id}/jobs"))
        .with_context(|| format!("build jobs URL for {repository} run {run_id}"))?;
    let (status, body, _rate_limit) = github_json_request_with_rate_limit(
        "GET",
        url.as_str(),
        token,
        None,
        GITHUB_PROBE_TIMEOUT_SECS,
    )
    .await
    .context("request queued workflow jobs")?;
    if !(200..300).contains(&status) {
        bail!(
            "queued workflow jobs request returned HTTP {status}: {}",
            body.trim()
        );
    }
    #[derive(Deserialize)]
    struct Jobs {
        jobs: Vec<ListedWorkflowJob>,
    }
    let jobs: Jobs = serde_json::from_str(&body).context("parse queued workflow jobs response")?;
    Ok(jobs
        .jobs
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
        .collect())
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

/// Remove live routing proof after a probe cannot establish current identity.
/// A prior valid `routing.json` must not survive without its source evidence.
pub fn invalidate_routing_evidence(state_dir: &Path) -> anyhow::Result<()> {
    for name in [ROUTING_EVIDENCE_FILE, ROUTING_FILE] {
        let path = state_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
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

/// Record a microVM executor proof bound to the packaged generation.
///
/// # Errors
/// Directory or write failures.
pub fn write_microvm_executor_ok(
    state_dir: &Path,
    generation: &crate::execution::MicroVmGeneration,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(EXECUTOR_OK);
    std::fs::write(&path, serde_json::to_vec(generation)?)?;
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
    fn derived_policy_refreshes_only_when_configuration_changes() {
        let dir = tmp("policy-change");
        let old = matching_fields();
        write_policy_if_changed(&dir, &old).unwrap();
        let old_mtime = std::fs::metadata(dir.join(ROUTING_POLICY_FILE))
            .unwrap()
            .modified()
            .unwrap();
        assert!(write_policy_if_changed(&dir, &old).is_ok());
        let mut updated = old.clone();
        updated.labels.push("velnor-trusted".into());
        write_policy_if_changed(&dir, &updated).unwrap();
        assert_eq!(read_policy(&dir), Some(updated));
        assert!(
            std::fs::metadata(dir.join(ROUTING_POLICY_FILE))
                .unwrap()
                .modified()
                .unwrap()
                >= old_mtime
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
        assert!(!observe_executor(
            &dir,
            velnor_model::ExecutionBackendKind::Docker
        ));
        write_executor_ok(&dir).unwrap();
        assert!(observe_executor(
            &dir,
            velnor_model::ExecutionBackendKind::Docker
        ));
        assert!(
            !observe_executor(&dir, velnor_model::ExecutionBackendKind::MicroVm),
            "stale ok\\n must not prove microvm"
        );
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
    fn invalidate_routing_evidence_removes_stale_proof_idempotently() {
        let dir = tmp("invalidate");
        let fields = matching_fields();
        write_routing_document(&dir, fields.clone(), fields.clone()).unwrap();
        write_evidence(&dir, &fields).unwrap();

        invalidate_routing_evidence(&dir).unwrap();
        assert!(!dir.join(ROUTING_FILE).exists());
        assert!(!dir.join(ROUTING_EVIDENCE_FILE).exists());
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());

        invalidate_routing_evidence(&dir).unwrap();
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
    fn repository_group_identity_requires_explicit_configuration() {
        assert_eq!(repository_group_identity(None), None);
    }

    #[test]
    fn repository_group_identity_accepts_explicit_name_or_id() {
        let policy = matching_fields();
        assert_eq!(
            repository_group_identity(Some(policy.group.as_str())),
            Some("velnor".into())
        );
        assert_eq!(repository_group_identity(Some("7")), Some("7".into()));
    }

    #[test]
    fn repository_group_identity_never_fabricates_default() {
        assert_ne!(repository_group_identity(None), Some("Default".into()));
    }

    #[test]
    fn explicit_policy_file_is_required_to_be_complete() {
        let dir = tmp("policy-file");
        let path = dir.join("desired.json");
        let fields = matching_fields();
        std::fs::write(&path, serde_json::to_vec(&fields).unwrap()).unwrap();
        assert_eq!(read_policy_file(&path).unwrap(), fields);
        std::fs::write(&path, br#"{"group":"velnor"}"#).unwrap();
        assert!(read_policy_file(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
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

        let snapshot = matching_fields();
        write_policy(&dir, &snapshot).unwrap();
        write_policy_if_absent(&dir, &policy).unwrap();
        assert_eq!(
            read_policy(&dir).unwrap().selected_repositories,
            snapshot.selected_repositories,
            "if-absent must not clobber an existing file"
        );
        write_policy(&dir, &policy).unwrap();
        assert_eq!(
            read_policy(&dir).unwrap().selected_repositories,
            policy.selected_repositories,
            "org fleets must replace a live-membership snapshot"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn org_probe_without_policy_writes_live_group_and_repos() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
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
                "total_count": 1,
                "runner_groups": [{
                    "id": 7,
                    "name": "velnor",
                    "default": false
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7,
                "name": "velnor",
                "default": false
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
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: None,
            configured_group: None,
            pool_id: Some(7),
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

    fn runner_groups() -> Vec<RunnerGroup> {
        vec![
            RunnerGroup {
                id: 7,
                name: "velnor".into(),
                default: false,
            },
            RunnerGroup {
                id: 8,
                name: "Default".into(),
                default: true,
            },
        ]
    }

    #[test]
    fn aggregate_rate_limit_status_preserves_stricter_endpoint_telemetry() {
        let runner_groups = GitHubRateLimitStatus {
            retry_after_seconds: Some(30),
            rate_limit_reset_epoch: Some(2_000),
            remaining: Some(0),
        };
        let repositories = GitHubRateLimitStatus {
            retry_after_seconds: None,
            rate_limit_reset_epoch: Some(1_500),
            remaining: Some(4_999),
        };

        assert_eq!(
            aggregate_rate_limit_status(runner_groups, repositories),
            runner_groups
        );
    }

    #[test]
    fn ordinary_permission_403_is_not_rate_limited() {
        let error = LiveLookupError::response(
            "runner-group repositories",
            403,
            "permission denied".to_owned(),
            GitHubRateLimitStatus {
                retry_after_seconds: None,
                rate_limit_reset_epoch: Some(2_000),
                remaining: Some(4_999),
            },
        );

        assert!(!error.rate_limited);
    }

    #[test]
    fn resolve_live_group_matches_exact_name() {
        assert_eq!(
            resolve_live_group(&runner_groups(), "velnor", None).map(|group| group.id),
            Ok(7)
        );
    }

    #[test]
    fn resolve_live_group_matches_exact_id() {
        assert_eq!(
            resolve_live_group(&runner_groups(), "", Some(7)).map(|group| group.name),
            Ok("velnor".to_owned())
        );
    }

    #[test]
    fn resolve_live_group_rejects_name_id_mismatch() {
        let result = resolve_live_group(&runner_groups(), "velnor", Some(8));
        assert!(matches!(
            result,
            Err(error) if error.contains("not configured id 8")
        ));
    }

    #[test]
    fn resolve_live_group_does_not_fallback_to_name_when_id_is_missing() {
        let result = resolve_live_group(&runner_groups(), "velnor", Some(9));
        assert!(matches!(
            result,
            Err(error) if error.contains("with id 9 was not found")
        ));
    }

    #[test]
    fn resolve_live_group_rejects_case_mismatch() {
        let result = resolve_live_group(&runner_groups(), "VELNOR", Some(7));
        assert!(matches!(result, Err(error) if error.contains("not configured id 7")));
    }

    #[test]
    fn resolve_live_group_rejects_missing_requested_identity() {
        let result = resolve_live_group(&runner_groups(), "missing", None);
        assert!(matches!(result, Err(error) if error.contains("was not found")));
    }

    #[test]
    fn resolve_live_group_rejects_when_no_identity_is_configured() {
        let result = resolve_live_group(&runner_groups(), "", None);
        assert!(matches!(
            result,
            Err(error) if error.contains("identity is missing")
        ));
    }

    #[test]
    fn resolve_live_group_rejects_empty_response() {
        let result = resolve_live_group(&[], "velnor", Some(7));
        assert!(matches!(
            result,
            Err(error) if error.contains("zero runner groups")
        ));
    }

    /// Secondary-limit 403 carrying `Retry-After` but no
    /// `x-ratelimit-reset`: the probe must surface a deadline derived from
    /// `Retry-After` so the controller paces to the requested delay instead
    /// of the fixed 600s fallback (end-to-end over the native transport).
    #[tokio::test]
    async fn rate_limited_probe_derives_reset_from_retry_after() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
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
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: None,
            configured_group: None,
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

    #[tokio::test]
    async fn org_probe_fails_closed_when_repository_lookup_fails() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"runners": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "runner_groups": [{"id": 7, "name": "velnor", "default": false}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7,
                "name": "velnor",
                "default": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let url = format!("{}/tailrocks", server.uri());
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: None,
            configured_group: None,
            pool_id: Some(7),
            configured_labels: &["velnor".into()],
            configured_trust: "trusted",
        })
        .await;
        assert!(probe.reachable);
        assert!(probe.evidence.is_none());
        assert!(probe
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("HTTP 503")));
    }

    #[tokio::test]
    async fn org_probe_does_not_accept_policy_repositories_when_live_response_is_empty() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"runners": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "runner_groups": [{"id": 7, "name": "velnor", "default": false}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7,
                "name": "velnor",
                "default": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0,
                "repositories": []
            })))
            .mount(&server)
            .await;

        let policy = RoutingFields {
            group: "velnor".into(),
            selected_repositories: vec!["tailrocks/stale-policy-repo".into()],
            labels: vec!["velnor".into()],
            trust_scope: "trusted".into(),
        };
        let url = format!("{}/tailrocks", server.uri());
        let probe = probe_github(GitHubProbeRequest {
            url: &url,
            token: "ghs_test",
            policy: Some(&policy),
            configured_group: None,
            pool_id: Some(7),
            configured_labels: &[],
            configured_trust: "trusted",
        })
        .await;

        assert!(probe.reachable);
        assert!(probe.evidence.is_none(), "probe={probe:?}");
        assert!(probe
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| { diagnostic.contains("zero runner-group repositories") }));
    }

    #[tokio::test]
    async fn queued_job_probe_preserves_valid_empty_queue() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/tailrocks/velnor/actions/runs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "workflow_runs": []
            })))
            .mount(&server)
            .await;
        let policy = matching_fields();
        let ids = queued_job_ids(
            &format!("{}/tailrocks/velnor", server.uri()),
            "token",
            &policy,
        )
        .await
        .unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn queued_job_probe_propagates_http_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/tailrocks/velnor/actions/runs"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
            .mount(&server)
            .await;
        let error = queued_job_ids(
            &format!("{}/tailrocks/velnor", server.uri()),
            "token",
            &matching_fields(),
        )
        .await
        .unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("HTTP 503"), "{chain}");
        assert!(chain.contains("upstream unavailable"), "{chain}");
    }

    #[tokio::test]
    async fn queued_job_probe_propagates_malformed_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/tailrocks/velnor/actions/runs"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&server)
            .await;
        let error = queued_job_ids(
            &format!("{}/tailrocks/velnor", server.uri()),
            "token",
            &matching_fields(),
        )
        .await
        .unwrap_err();
        let chain = format!("{error:#}");
        assert!(
            chain.contains("parse queued workflow runs response"),
            "{chain}"
        );
    }
}
