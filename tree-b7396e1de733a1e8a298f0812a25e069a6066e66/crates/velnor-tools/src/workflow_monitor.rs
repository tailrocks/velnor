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
const MAX_EVIDENCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOCAL_ERRORS: usize = 16;
const MAX_LOCAL_ERROR_BYTES: usize = 512;
const MAX_LOCAL_SNAPSHOT_BYTES: usize = 256 * 1024;
const MAX_MONITOR_OBSERVATIONS: usize = 2048;
const MAX_MONITOR_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LOCAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_PAGE_LIMIT: u32 = 100;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Configuration for monitoring one GitHub Actions run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMonitorConfig {
    /// GitHub `OWNER/REPOSITORY` slug.
    pub repo: String,
    /// GitHub Actions run database id.
    pub run_id: u64,
    /// Cooperative polling/local-observation budget. Scheduler delay and
    /// evidence serialization or filesystem operations may extend return time.
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
        if self.timeout.is_zero() {
            bail!("monitor timeout must be greater than zero");
        }
        if self.timeout > MAX_MONITOR_TIMEOUT {
            bail!("monitor timeout must not exceed 24 hours");
        }
        if self.poll_interval < MIN_POLL_INTERVAL {
            bail!("poll interval must be at least 100 milliseconds");
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
///
/// The timeout is cooperative and observation-capped, not a hard function
/// return deadline; evidence persistence may take additional time.
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
    let mut observation_count = 0;

    let mut first_poll = true;
    let (final_run, timed_out) = loop {
        if !first_poll && Instant::now() >= deadline {
            let final_run = observations
                .last()
                .map(|observation| observation.run.clone())
                .context("monitor timed out before observing a run")?;
            break (final_run, true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let run = fetch_run(&config.repo, config.run_id, remaining)?;
        let response_acquired_at = Instant::now();
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
        if observations.len() == MAX_MONITOR_OBSERVATIONS {
            observations.remove(0);
        }
        observations.push(observation);
        observation_count += 1;
        write_current_evidence(
            evidence_path,
            &config.repo,
            config.run_id,
            &started_at,
            false,
            &mut observations,
        )?;
        if current.is_terminal() {
            break (current, response_acquired_at >= deadline);
        }
        if observation_count >= MAX_MONITOR_OBSERVATIONS || Instant::now() >= deadline {
            break (current, true);
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
            &mut observations,
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
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return unavailable_velnor_observation(instance, "monitor deadline exhausted");
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return unavailable_velnor_observation(
                instance,
                format!("build local Velnor observation runtime: {error}"),
            );
        }
    };
    match runtime.block_on(tokio::time::timeout(
        remaining,
        collect_velnor_snapshot(&instance, deadline),
    )) {
        Ok(Ok(observation)) => observation,
        Ok(Err(error)) => unavailable_velnor_observation(instance, error.to_string()),
        Err(_) => unavailable_velnor_observation(instance, "monitor deadline exhausted"),
    }
}

async fn collect_velnor_snapshot(instance: &str, deadline: Instant) -> Result<VelnorObservation> {
    let endpoint = UnixEndpoint::from_instance(instance).context("validate Velnor instance")?;
    let timeout = LOCAL_REQUEST_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
    if timeout.is_zero() {
        bail!("monitor deadline exhausted before local snapshot");
    }
    let client = UnixControlClient::new(endpoint).with_timeout(timeout);
    let query = ResourceQuery {
        limit: Some(LOCAL_PAGE_LIMIT),
        ..ResourceQuery::default()
    };
    let mut snapshots = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    let mut snapshot_budget = MAX_LOCAL_SNAPSHOT_BYTES;
    match client.info().await {
        Ok(info) => {
            insert_local_snapshot(
                &mut snapshots,
                &mut errors,
                &mut snapshot_budget,
                "status".to_owned(),
                serde_json::json!({
                    "api_version": info.api_version,
                    "schema_version": info.schema_version,
                    "mutations": info.mutations,
                }),
            );
        }
        Err(error) => record_local_error(&mut errors, "status", error),
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
            record_local_error(&mut errors, key, "monitor deadline exhausted");
            continue;
        }
        match client.get_resources(resource, &query).await {
            Ok(page) => {
                insert_local_snapshot(
                    &mut snapshots,
                    &mut errors,
                    &mut snapshot_budget,
                    key.to_owned(),
                    serde_json::json!({
                        "resources": page.resources,
                        "next_page_token": page.next_page_token,
                    }),
                );
            }
            Err(error) => record_local_error(&mut errors, key, error),
        }
    }
    if deadline > Instant::now() {
        match client.watch(None, None, Some(LOCAL_PAGE_LIMIT)).await {
            Ok(events) => {
                let value = match serde_json::to_value(events) {
                    Ok(value) => value,
                    Err(error) => {
                        record_local_error(&mut errors, "events", error);
                        Value::Null
                    }
                };
                if !value.is_null() {
                    insert_local_snapshot(
                        &mut snapshots,
                        &mut errors,
                        &mut snapshot_budget,
                        "events".to_owned(),
                        value,
                    );
                }
            }
            Err(error) => record_local_error(&mut errors, "events", error),
        }
    } else {
        record_local_error(&mut errors, "events", "monitor deadline exhausted");
    }
    Ok(VelnorObservation {
        instance: instance.to_owned(),
        available: errors.is_empty(),
        snapshots,
        errors,
    })
}

fn insert_local_snapshot(
    snapshots: &mut std::collections::BTreeMap<String, Value>,
    errors: &mut Vec<String>,
    budget: &mut usize,
    key: String,
    value: Value,
) {
    let size = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes.len(),
        Err(error) => {
            record_local_error(errors, &key, error);
            return;
        }
    };
    if size > *budget {
        record_local_error(
            errors,
            &key,
            format!("snapshot exceeds the {MAX_LOCAL_SNAPSHOT_BYTES}-byte evidence budget"),
        );
        return;
    }
    *budget -= size;
    snapshots.insert(key, value);
}

fn record_local_error(errors: &mut Vec<String>, key: &str, error: impl std::fmt::Display) {
    if errors.len() >= MAX_LOCAL_ERRORS {
        return;
    }
    let message = format!("{key}: {error}");
    errors.push(truncate_utf8(message, MAX_LOCAL_ERROR_BYTES));
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.saturating_sub("…".len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push('…');
    value
}

fn unavailable_velnor_observation(instance: String, error: impl Into<String>) -> VelnorObservation {
    VelnorObservation {
        instance,
        available: false,
        snapshots: std::collections::BTreeMap::new(),
        errors: vec![truncate_utf8(error.into(), MAX_LOCAL_ERROR_BYTES)],
    }
}

fn write_current_evidence(
    path: Option<&Path>,
    repo: &str,
    run_id: u64,
    started_at: &str,
    timed_out: bool,
    observations: &mut Vec<WorkflowRunObservation>,
) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    let updated_at = utc_timestamp()?;
    trim_observations_to_evidence_budget(
        repo,
        run_id,
        started_at,
        &updated_at,
        timed_out,
        observations,
    )?;
    let evidence = WorkflowMonitorEvidence {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        repo: repo.to_owned(),
        run_id,
        started_at: started_at.to_owned(),
        updated_at,
        timed_out,
        observations: observations.clone(),
    };
    write_evidence(path, &evidence)
}

fn trim_observations_to_evidence_budget(
    repo: &str,
    run_id: u64,
    started_at: &str,
    updated_at: &str,
    timed_out: bool,
    observations: &mut Vec<WorkflowRunObservation>,
) -> Result<()> {
    loop {
        let evidence = WorkflowMonitorEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            repo: repo.to_owned(),
            run_id,
            started_at: started_at.to_owned(),
            updated_at: updated_at.to_owned(),
            timed_out,
            observations: observations.clone(),
        };
        let size = serde_json::to_vec_pretty(&evidence)
            .context("serialize workflow evidence for size limit")?
            .len();
        if size <= MAX_EVIDENCE_BYTES {
            return Ok(());
        }
        if observations.len() <= 1 {
            bail!("single workflow observation exceeds {MAX_EVIDENCE_BYTES}-byte evidence limit");
        }
        observations.remove(0);
    }
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
        if bytes.len() > MAX_EVIDENCE_BYTES {
            bail!("workflow evidence exceeds {MAX_EVIDENCE_BYTES}-byte limit");
        }
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
    fn validation_rejects_unbounded_monitor_settings() {
        assert!(WorkflowMonitorConfig::new("owner/repo", 1)
            .with_timeout(MAX_MONITOR_TIMEOUT + Duration::from_secs(1))
            .validate()
            .is_err());
        assert!(WorkflowMonitorConfig::new("owner/repo", 1)
            .with_poll_interval(MIN_POLL_INTERVAL - Duration::from_millis(1))
            .validate()
            .is_err());
    }

    #[test]
    fn local_snapshots_and_errors_stay_bounded() {
        let mut snapshots = std::collections::BTreeMap::new();
        let mut errors = Vec::new();
        let mut budget = MAX_LOCAL_SNAPSHOT_BYTES;
        insert_local_snapshot(
            &mut snapshots,
            &mut errors,
            &mut budget,
            "oversized".to_owned(),
            serde_json::json!("x".repeat(MAX_LOCAL_SNAPSHOT_BYTES + 1)),
        );
        for _ in 0..(MAX_LOCAL_ERRORS + 4) {
            record_local_error(
                &mut errors,
                "resource",
                "x".repeat(MAX_LOCAL_ERROR_BYTES + 1),
            );
        }
        assert!(snapshots.is_empty());
        assert!(budget == MAX_LOCAL_SNAPSHOT_BYTES);
        assert_eq!(errors.len(), MAX_LOCAL_ERRORS);
        assert!(errors
            .iter()
            .all(|error| error.len() <= MAX_LOCAL_ERROR_BYTES));
    }

    #[test]
    fn expired_deadline_does_not_start_local_observation() {
        let started = Instant::now();
        let observation = collect_velnor_observation("velnor", started);
        assert!(!observation.available);
        assert_eq!(observation.snapshots.len(), 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn evidence_history_is_trimmed_to_byte_budget() {
        let run = GitHubActionsRun {
            id: 1,
            status: "in_progress".to_owned(),
            conclusion: None,
            name: Some("x".repeat(256 * 1024)),
            html_url: None,
            head_sha: None,
            head_branch: None,
            workflow_path: None,
        };
        let mut observations = (0..40)
            .map(|_| WorkflowRunObservation {
                observed_at: "2026-01-01T00:00:00Z".to_owned(),
                run: run.clone(),
                velnor: None,
            })
            .collect::<Vec<_>>();
        trim_observations_to_evidence_budget(
            "owner/repo",
            1,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
            &mut observations,
        )
        .unwrap();
        let evidence = WorkflowMonitorEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            repo: "owner/repo".to_owned(),
            run_id: 1,
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            timed_out: false,
            observations,
        };
        assert!(serde_json::to_vec(&evidence).unwrap().len() <= MAX_EVIDENCE_BYTES);
        assert!(evidence.observations.len() < 40);
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
