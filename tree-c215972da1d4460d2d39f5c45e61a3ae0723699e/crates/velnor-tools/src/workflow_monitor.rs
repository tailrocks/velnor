//! Read-only correlation of GitHub workflow state and local Velnor evidence.

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use velnor_client::{ResourceQuery, UnixControlClient, UnixEndpoint};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const GITHUB_API_URL: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const GITHUB_USER_AGENT: &str = "velnor-tools-workflow-monitor";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Configuration for monitoring one GitHub Actions run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMonitorConfig {
    /// GitHub `OWNER/REPOSITORY` slug.
    pub repo: String,
    /// GitHub Actions run database id.
    pub run_id: u64,
    /// Maximum wall-clock monitoring time.
    pub timeout: Duration,
    /// Delay between observations.
    pub poll_interval: Duration,
    /// Optional directory receiving atomic JSON evidence.
    pub evidence_dir: Option<PathBuf>,
    /// Optional local Velnor instance to observe through the typed control client.
    pub instance: Option<String>,
    evidence_path: Option<PathBuf>,
}

impl WorkflowMonitorConfig {
    /// Create a configuration with fifteen-minute monitoring and two-second polling.
    #[must_use]
    pub fn new(repo: impl Into<String>, run_id: u64) -> Self {
        Self {
            repo: repo.into(),
            run_id,
            timeout: Duration::from_secs(15 * 60),
            poll_interval: Duration::from_secs(2),
            evidence_dir: None,
            instance: None,
            evidence_path: None,
        }
    }

    /// Set the maximum monitoring duration.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the delay between observations.
    #[must_use]
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Set the evidence output directory.
    #[must_use]
    pub fn with_evidence_dir(mut self, evidence_dir: impl Into<PathBuf>) -> Self {
        let evidence_dir = evidence_dir.into();
        self.evidence_path =
            Some(evidence_dir.join(format!("github-actions-run-{}.json", self.run_id)));
        self.evidence_dir = Some(evidence_dir);
        self
    }

    /// Observe one local Velnor instance on each poll.
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    fn validate(&self) -> Result<()> {
        validate_repo_slug(&self.repo)?;
        if self.run_id == 0 {
            bail!("run id must be greater than zero");
        }
        if self.timeout.is_zero() || self.poll_interval.is_zero() {
            bail!("monitor timeout and poll interval must be greater than zero");
        }
        if self.instance.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 64
                || !value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
                || !value.as_bytes()[0].is_ascii_lowercase()
                    && !value.as_bytes()[0].is_ascii_digit()
        }) {
            bail!("instance must match [a-z0-9][a-z0-9_-]{{0,63}}");
        }
        Ok(())
    }

    fn evidence_path(&self) -> Option<&Path> {
        self.evidence_path.as_deref()
    }
}

/// GitHub's workflow-run observation, retaining no credential-bearing fields.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct GitHubActionsRun {
    /// Immutable database id.
    pub id: u64,
    /// Current status token.
    pub status: String,
    /// Terminal conclusion, when available.
    #[serde(default)]
    pub conclusion: Option<String>,
    /// Optional workflow metadata.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    #[serde(rename = "path")]
    pub workflow_path: Option<String>,
}

impl GitHubActionsRun {
    /// Whether GitHub reported a terminal run.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status == "completed"
    }

    /// Whether GitHub reported success.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.is_terminal() && self.conclusion.as_deref() == Some("success")
    }
}

/// One timestamped observation pair.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkflowRunObservation {
    /// UTC observation time.
    pub observed_at: String,
    /// GitHub authoritative snapshot.
    pub run: GitHubActionsRun,
    /// Optional local Velnor snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velnor: Option<VelnorObservation>,
}

/// Bounded, read-only local observation gathered through `velnor-client`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct VelnorObservation {
    /// Selected daemon instance.
    pub instance: String,
    /// Whether every local query succeeded.
    pub available: bool,
    /// JSON responses keyed by resource query.
    pub snapshots: std::collections::BTreeMap<String, Value>,
    /// Sanitized command errors, when a local query was unavailable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Atomically rewritten monitor evidence document.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkflowMonitorEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// GitHub repository slug.
    pub repo: String,
    /// GitHub run database id.
    pub run_id: u64,
    /// UTC monitor start time.
    pub started_at: String,
    /// UTC latest write time.
    pub updated_at: String,
    /// Whether timeout ended observation before terminal state.
    pub timed_out: bool,
    /// Ordered observations.
    pub observations: Vec<WorkflowRunObservation>,
}

/// Result after terminal observation or timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMonitorResult<'a> {
    /// Last GitHub snapshot.
    pub final_run: GitHubActionsRun,
    /// Ordered observations.
    pub observations: Vec<WorkflowRunObservation>,
    /// Whether the monitor deadline elapsed.
    pub timed_out: bool,
    /// Evidence path, if configured.
    pub evidence_path: Option<&'a Path>,
}

impl<'a> WorkflowMonitorResult<'a> {
    /// Whether the run completed successfully before timeout.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.final_run.succeeded()
    }
}

/// Poll GitHub until terminal state or deadline, optionally capturing Velnor state.
pub fn monitor_workflow_run<'a>(
    config: &'a WorkflowMonitorConfig,
) -> Result<WorkflowMonitorResult<'a>> {
    config.validate()?;
    let started_at = utc_timestamp()?;
    let deadline = Instant::now()
        .checked_add(config.timeout)
        .context("monitor timeout exceeds instant range")?;
    let evidence_path = config.evidence_path();
    let mut observations: Vec<WorkflowRunObservation> = Vec::new();
    let mut timed_out = false;

    let mut first_poll = true;
    let final_run = loop {
        if !first_poll && Instant::now() >= deadline {
            timed_out = true;
            break observations
                .last()
                .map(|observation| observation.run.clone())
                .context("monitor timed out before observing a run")?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let run = fetch_run(&config.repo, config.run_id, remaining)?;
        first_poll = false;
        if run.id != config.run_id {
            bail!(
                "GitHub returned run {} for requested run {}",
                run.id,
                config.run_id
            );
        }
        let observation = WorkflowRunObservation {
            observed_at: utc_timestamp()?,
            run,
            velnor: config
                .instance
                .as_deref()
                .map(|instance| collect_velnor_observation(instance, deadline)),
        };
        let current = observation.run.clone();
        observations.push(observation);
        write_current_evidence(
            evidence_path,
            &config.repo,
            config.run_id,
            &started_at,
            false,
            &observations,
        )?;
        if current.is_terminal() {
            break current;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break current;
        }
        thread::sleep(
            config
                .poll_interval
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    };

    if timed_out {
        write_current_evidence(
            evidence_path,
            &config.repo,
            config.run_id,
            &started_at,
            true,
            &observations,
        )?;
    }
    Ok(WorkflowMonitorResult {
        final_run,
        observations,
        timed_out,
        evidence_path,
    })
}

fn fetch_run(repo: &str, run_id: u64, timeout: Duration) -> Result<GitHubActionsRun> {
    if timeout.is_zero() {
        bail!("GitHub request timeout must be greater than zero");
    }

    let token = github_token()?;
    let endpoint = format!("{GITHUB_API_URL}/repos/{repo}/actions/runs/{run_id}");
    let client = Client::builder()
        .timeout(timeout.min(GITHUB_REQUEST_TIMEOUT))
        .user_agent(GITHUB_USER_AGENT)
        .build()
        .context("build GitHub workflow monitor HTTP client")?;
    let response = client
        .get(&endpoint)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send()
        .with_context(|| format!("fetch GitHub Actions run {run_id} for {repo}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("GitHub Actions run {run_id} request failed with HTTP {status}");
    }
    response
        .json()
        .with_context(|| format!("parse GitHub Actions run {run_id} response"))
}

fn github_token() -> Result<String> {
    for variable in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(variable)
            && !value.trim().is_empty()
        {
            return Ok(value);
        }
    }
    bail!("workflow monitor requires a GitHub token in GITHUB_TOKEN (or GH_TOKEN); none found")
}

fn collect_velnor_observation(instance: &str, deadline: Instant) -> VelnorObservation {
    let instance = instance.to_owned();
    let worker_instance = instance.clone();
    let thread = thread::Builder::new()
        .name("velnor-workflow-monitor".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build local Velnor observation runtime")?;
            runtime.block_on(collect_velnor_snapshot(&worker_instance, deadline))
        });
    match thread {
        Ok(thread) => match thread.join() {
            Ok(Ok(observation)) => observation,
            Ok(Err(error)) => unavailable_velnor_observation(instance, error.to_string()),
            Err(_) => unavailable_velnor_observation(instance, "observation thread panicked"),
        },
        Err(error) => {
            unavailable_velnor_observation(instance, format!("spawn observation thread: {error}"))
        }
    }
}

async fn collect_velnor_snapshot(instance: &str, deadline: Instant) -> Result<VelnorObservation> {
    let endpoint = UnixEndpoint::from_instance(instance).context("validate Velnor instance")?;
    let timeout = Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now()));
    if timeout.is_zero() {
        bail!("monitor deadline exhausted before local snapshot");
    }
    let client = UnixControlClient::new(endpoint).with_timeout(timeout);
    let query = ResourceQuery {
        limit: Some(100),
        ..ResourceQuery::default()
    };
    let mut snapshots = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    match client.info().await {
        Ok(info) => {
            snapshots.insert(
                "status".to_owned(),
                serde_json::json!({
                    "api_version": info.api_version,
                    "schema_version": info.schema_version,
                    "mutations": info.mutations,
                }),
            );
        }
        Err(error) => errors.push(format!("status: {error}")),
    }
    for (key, resource) in [
        ("hosts", "hosts"),
        ("instances", "instances"),
        ("runners", "runners"),
        ("slots", "slots"),
        ("jobs", "jobs"),
        ("runs", "runs"),
    ] {
        if deadline <= Instant::now() {
            errors.push(format!("{key}: monitor deadline exhausted"));
            continue;
        }
        match client.get_resources(resource, &query).await {
            Ok(page) => {
                snapshots.insert(
                    key.to_owned(),
                    serde_json::json!({
                        "resources": page.resources,
                        "next_page_token": page.next_page_token,
                    }),
                );
            }
            Err(error) => errors.push(format!("{key}: {error}")),
        }
    }
    if deadline > Instant::now() {
        match client.watch(None, None, Some(100)).await {
            Ok(events) => {
                snapshots.insert(
                    "events".to_owned(),
                    serde_json::to_value(events).context("serialize Velnor events")?,
                );
            }
            Err(error) => errors.push(format!("events: {error}")),
        }
    } else {
        errors.push("events: monitor deadline exhausted".to_owned());
    }
    Ok(VelnorObservation {
        instance: instance.to_owned(),
        available: errors.is_empty(),
        snapshots,
        errors,
    })
}

fn unavailable_velnor_observation(instance: String, error: impl Into<String>) -> VelnorObservation {
    VelnorObservation {
        instance,
        available: false,
        snapshots: std::collections::BTreeMap::new(),
        errors: vec![error.into()],
    }
}

fn write_current_evidence(
    path: Option<&Path>,
    repo: &str,
    run_id: u64,
    started_at: &str,
    timed_out: bool,
    observations: &[WorkflowRunObservation],
) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    let evidence = WorkflowMonitorEvidence {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        repo: repo.to_owned(),
        run_id,
        started_at: started_at.to_owned(),
        updated_at: utc_timestamp()?,
        timed_out,
        observations: observations.to_owned(),
    };
    write_evidence(path, &evidence)
}

fn write_evidence(path: &Path, evidence: &WorkflowMonitorEvidence) -> Result<()> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("evidence path must have a UTF-8 file name")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create evidence directory {}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(evidence).context("serialize workflow evidence")?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create temporary evidence file {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write temporary evidence file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flush temporary evidence file {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("atomically replace evidence file {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_repo_slug(repo: &str) -> Result<()> {
    let Some((owner, repository)) = repo.split_once('/') else {
        bail!("repository must be an OWNER/REPOSITORY slug")
    };
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || !owner.bytes().all(valid_slug_byte)
        || !repository.bytes().all(valid_slug_byte)
    {
        bail!("repository must be an OWNER/REPOSITORY slug")
    }
    Ok(())
}

fn valid_slug_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn utc_timestamp() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("format UTC timestamp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_injection_and_zero_values() {
        assert!(WorkflowMonitorConfig::new("owner/repo/other", 1)
            .validate()
            .is_err());
        assert!(WorkflowMonitorConfig::new("owner/repo", 0)
            .validate()
            .is_err());
        assert!(WorkflowMonitorConfig::new("owner/repo", 1)
            .with_instance("../other")
            .validate()
            .is_err());
    }

    #[test]
    fn success_requires_terminal_success() {
        let run = GitHubActionsRun {
            id: 1,
            status: "in_progress".to_owned(),
            conclusion: Some("success".to_owned()),
            name: None,
            html_url: None,
            head_sha: None,
            head_branch: None,
            workflow_path: None,
        };
        assert!(!run.succeeded());
    }
}
