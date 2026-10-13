#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, USER_AGENT},
    Client, Method, StatusCode,
};
use rsa::{
    pkcs8::{EncodePrivateKey, LineEnding},
    rand_core::OsRng,
    traits::PublicKeyParts,
    BigUint, RsaPrivateKey,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;
use uuid::Uuid;

/// GitHub Actions runner protocol version Velnor implements.
pub const RUNNER_VERSION: &str = "2.337.0";
pub const RUNNER_USER_AGENT: &str = "actions-runner/2.337.0 (velnor)";
/// Velnor's own version, sourced from Cargo.toml.
pub const VELNOR_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Display name shown in "Set up job" log: "Velnor Runner/<version> (protocol: <runner_version>)"
pub fn velnor_runner_display() -> String {
    format!("Velnor Runner/{VELNOR_VERSION} (protocol: {RUNNER_VERSION})")
}
pub const EMPTY_LOCK_TOKEN: &str = "00000000-0000-0000-0000-000000000000";
const JIT_CURL_CONNECT_TIMEOUT_SECONDS: u64 = 10;
const JIT_CURL_MAX_TIME_SECONDS: u64 = 45;

#[derive(Debug, thiserror::Error)]
#[error("{action} failed: status={status}, body={body}")]
pub struct GitHubApiError {
    pub status: u16,
    pub action: String,
    pub body: String,
    pub retry_after_seconds: Option<u64>,
    pub rate_limit_reset_epoch: Option<u64>,
    /// `x-ratelimit-remaining`. Required to tell quota 403 (remaining=0)
    /// from permission 403 (remaining>0); GitHub sends reset headers on both.
    pub remaining: Option<u64>,
}

/// GitHub DELETE `/actions/runners/{id}` while the runner still holds a job.
///
/// HTTP 422 here is not a missing registration. Hammering DELETE or dropping
/// the local JIT identity churns new runner IDs and leaves GitHub `offline+busy`.
#[derive(Debug, thiserror::Error)]
#[error(
    "GitHub refused to delete runner: currently running a job (HTTP 422); quarantine until the job is terminal; local identity preserved: {0}"
)]
pub(crate) struct RunnerBusyConflict(pub(crate) String);

/// Outcome of a runner DELETE that the supervisor can act on without another HTTP round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunnerDeleteOutcome {
    /// 204 or 404: registration is gone or never existed.
    Gone,
    /// 422: GitHub still believes a job is running on this runner.
    BusyConflict,
}

pub(crate) fn runner_delete_is_busy_conflict(status: u16, body: &str) -> bool {
    if status != 422 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("currently running a job")
        || lower.contains("unable to delete")
        || lower.contains("runner is busy")
        || lower.contains("runner_is_busy")
}

pub(crate) fn classify_runner_delete(status: u16, body: &str) -> Option<RunnerDeleteOutcome> {
    match status {
        204 | 404 => Some(RunnerDeleteOutcome::Gone),
        _ if runner_delete_is_busy_conflict(status, body) => {
            Some(RunnerDeleteOutcome::BusyConflict)
        }
        _ => None,
    }
}

fn github_api_error(
    action: impl Into<String>,
    status: u16,
    body: impl Into<String>,
) -> anyhow::Error {
    GitHubApiError {
        status,
        action: action.into(),
        body: body.into(),
        retry_after_seconds: None,
        rate_limit_reset_epoch: None,
        remaining: None,
    }
    .into()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GitHubRetryHint {
    retry_after_seconds: Option<u64>,
    rate_limit_reset_epoch: Option<u64>,
    remaining: Option<u64>,
}

/// Rate-limit telemetry captured from GitHub response headers. Read-only
/// probes report it so callers can pace themselves instead of hammering a
/// token that is already exhausted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitHubRateLimitStatus {
    pub retry_after_seconds: Option<u64>,
    pub rate_limit_reset_epoch: Option<u64>,
    pub remaining: Option<u64>,
}

impl From<GitHubRetryHint> for GitHubRateLimitStatus {
    fn from(hint: GitHubRetryHint) -> Self {
        Self {
            retry_after_seconds: hint.retry_after_seconds,
            rate_limit_reset_epoch: hint.rate_limit_reset_epoch,
            remaining: hint.remaining,
        }
    }
}

impl GitHubRateLimitStatus {
    /// True when the response says the shared token budget is exhausted.
    ///
    /// GitHub sends `x-ratelimit-remaining`/`-reset` on every response,
    /// including permission 403s, so a bare header pair is NOT proof of a
    /// rate limit. Exhaustion is: 429, `remaining: 0`, or an explicit
    /// `Retry-After` on a 403 (secondary/abuse limit).
    #[must_use]
    pub fn is_limited(self, status: u16) -> bool {
        if status == 429 {
            return true;
        }
        if status != 403 {
            return false;
        }
        self.remaining == Some(0) || self.retry_after_seconds.is_some()
    }

    /// Absolute wait epoch: `x-ratelimit-reset` when present, otherwise
    /// `now + Retry-After` for secondary/abuse 403s that omit the reset header.
    #[must_use]
    pub fn reset_epoch_or_retry_after(self, now_epoch: u64) -> Option<u64> {
        self.rate_limit_reset_epoch.or_else(|| {
            self.retry_after_seconds
                .map(|seconds| now_epoch.saturating_add(seconds))
        })
    }
}

impl GitHubRetryHint {
    fn delay(self, now_epoch: u64) -> Option<std::time::Duration> {
        let retry_after = self.retry_after_seconds.unwrap_or(0);
        let until_reset = self
            .rate_limit_reset_epoch
            .map(|reset| reset.saturating_sub(now_epoch))
            .unwrap_or(0);
        let seconds = retry_after.max(until_reset);
        (seconds > 0).then(|| std::time::Duration::from_secs(seconds))
    }
}

fn parse_github_retry_headers(headers: &[u8]) -> GitHubRetryHint {
    let mut hint = GitHubRetryHint::default();
    for line in String::from_utf8_lossy(headers).lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("retry-after") {
            hint.retry_after_seconds = value.parse().ok();
        } else if name.eq_ignore_ascii_case("x-ratelimit-reset") {
            hint.rate_limit_reset_epoch = value.parse().ok();
        } else if name.eq_ignore_ascii_case("x-ratelimit-remaining") {
            hint.remaining = value.parse().ok();
        }
    }
    hint
}

fn github_retry_hint_from_header_map(headers: &HeaderMap) -> GitHubRetryHint {
    let parse = |name: &'static str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    };
    GitHubRetryHint {
        retry_after_seconds: parse("retry-after"),
        rate_limit_reset_epoch: parse("x-ratelimit-reset"),
        remaining: parse("x-ratelimit-remaining"),
    }
}

fn github_api_error_with_retry(
    action: impl Into<String>,
    status: u16,
    body: impl Into<String>,
    hint: GitHubRetryHint,
) -> anyhow::Error {
    GitHubApiError {
        status,
        action: action.into(),
        body: body.into(),
        retry_after_seconds: hint.retry_after_seconds,
        rate_limit_reset_epoch: hint.rate_limit_reset_epoch,
        remaining: hint.remaining,
    }
    .into()
}

impl GitHubApiError {
    #[must_use]
    pub fn rate_limit_status(&self) -> GitHubRateLimitStatus {
        GitHubRateLimitStatus {
            retry_after_seconds: self.retry_after_seconds,
            rate_limit_reset_epoch: self.rate_limit_reset_epoch,
            remaining: self.remaining,
        }
    }
}

pub fn github_api_retry_delay(error: &anyhow::Error) -> Option<std::time::Duration> {
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .and_then(|error| {
            GitHubRetryHint {
                retry_after_seconds: error.retry_after_seconds,
                rate_limit_reset_epoch: error.rate_limit_reset_epoch,
                remaining: error.remaining,
            }
            .delay(now_epoch)
        })
}

/// Quota-limited 403/429 telemetry. Permission 403s with remaining > 0
/// return `None` so they cannot hold the whole fleet.
#[must_use]
pub fn github_api_quota_status(error: &anyhow::Error) -> Option<GitHubRateLimitStatus> {
    error.chain().find_map(|cause| {
        let api = cause.downcast_ref::<GitHubApiError>()?;
        let status = api.rate_limit_status();
        status.is_limited(api.status).then_some(status)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubScope {
    pub original_url: String,
    pub hosted: bool,
    pub api_base_url: Url,
    pub jit_config_url: Url,
    runner_scope_path: String,
}

impl GitHubScope {
    pub fn parse(input: &str) -> Result<Self> {
        let url = Url::parse(input).with_context(|| format!("parse GitHub URL '{input}'"))?;
        let host = url.host_str().context("GitHub URL needs host")?;
        let hosted = is_hosted_github(host);
        let segments: Vec<_> = url
            .path_segments()
            .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
            .unwrap_or_default();

        if segments.len() != 1 && segments.len() != 2 {
            bail!("GitHub URL must point to org, repo, or enterprise scope");
        }

        let api_base_url = api_base_url(&url, hosted)?;
        let token_scope = token_scope_path(&segments)?;
        let jit_config_url =
            api_base_url.join(&format!("{token_scope}/actions/runners/generate-jitconfig"))?;

        Ok(Self {
            original_url: input.to_string(),
            hosted,
            api_base_url,
            jit_config_url,
            runner_scope_path: token_scope,
        })
    }

    pub fn runner_url(&self, runner_id: i64) -> Result<Url> {
        self.api_base_url
            .join(&format!(
                "{}/actions/runners/{runner_id}",
                self.runner_scope_path
            ))
            .context("build GitHub runner URL")
    }

    pub fn runners_url(&self) -> Result<Url> {
        self.api_base_url
            .join(&format!("{}/actions/runners", self.runner_scope_path))
            .context("build GitHub runners URL")
    }

    pub fn runner_groups_url(&self) -> Result<Url> {
        if !self.runner_scope_path.starts_with("orgs/")
            && !self.runner_scope_path.starts_with("enterprises/")
        {
            bail!("runner groups apply only to organization or enterprise scopes");
        }
        self.api_base_url
            .join(&format!("{}/actions/runner-groups", self.runner_scope_path))
            .context("build GitHub runner groups URL")
    }

    pub fn runner_group_url(&self, group_id: i64) -> Result<Url> {
        if !self.runner_scope_path.starts_with("orgs/")
            && !self.runner_scope_path.starts_with("enterprises/")
        {
            bail!("runner groups apply only to organization or enterprise scopes");
        }
        self.api_base_url
            .join(&format!(
                "{}/actions/runner-groups/{group_id}",
                self.runner_scope_path
            ))
            .context("build GitHub runner group URL")
    }

    pub fn runner_group_repositories_url(&self, group_id: i64) -> Result<Url> {
        if !self.runner_scope_path.starts_with("orgs/")
            && !self.runner_scope_path.starts_with("enterprises/")
        {
            bail!("runner groups apply only to organization or enterprise scopes");
        }
        self.api_base_url
            .join(&format!(
                "{}/actions/runner-groups/{group_id}/repositories",
                self.runner_scope_path
            ))
            .context("build GitHub runner group repositories URL")
    }

    pub fn kind(&self) -> &'static str {
        if self.runner_scope_path.starts_with("orgs/") {
            "organization"
        } else if self.runner_scope_path.starts_with("enterprises/") {
            "enterprise"
        } else {
            "repository"
        }
    }

    pub fn org_login(&self) -> Option<&str> {
        self.runner_scope_path.strip_prefix("orgs/")
    }

    pub fn repo_full_name(&self) -> Option<(&str, &str)> {
        self.runner_scope_path
            .strip_prefix("repos/")
            .and_then(|rest| rest.split_once('/'))
    }

    pub fn workflow_run_cancel_url(&self, repository: &str, run_id: u64) -> Result<Url> {
        self.api_base_url
            .join(&format!("repos/{repository}/actions/runs/{run_id}/cancel"))
            .context("build GitHub workflow run cancel URL")
    }

    pub fn repo_queued_runs_url(&self, repository: &str) -> Result<Url> {
        self.api_base_url
            .join(&format!("repos/{repository}/actions/runs"))
            .context("build GitHub queued workflow runs URL")
    }

    pub fn org_repos_url(&self) -> Result<Url> {
        let org = self
            .org_login()
            .ok_or_else(|| anyhow::anyhow!("org repos URL requires organization scope"))?;
        self.api_base_url
            .join(&format!("orgs/{org}/repos"))
            .context("build GitHub org repos URL")
    }
}

/// GitHub REST job waiting in `queued` with no runner assigned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ListedWorkflowJob {
    pub id: u64,
    pub run_id: u64,
    #[serde(default)]
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub runner_id: Option<i64>,
    pub created_at: Option<String>,
    pub run_url: Option<String>,
}

pub(crate) fn repository_from_actions_run_url(run_url: &str) -> Option<String> {
    let rest = run_url.split("/repos/").nth(1)?;
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// 202 accepted; 409/404 already terminal.
pub(crate) fn classify_workflow_cancel(status: u16) -> bool {
    matches!(status, 202 | 409 | 404)
}

fn is_hosted_github(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com")
}

fn api_base_url(github_url: &Url, hosted: bool) -> Result<Url> {
    let host = github_url.host_str().context("GitHub URL needs host")?;
    let hostport = match github_url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    if hosted {
        Url::parse(&format!("{}://api.{hostport}/", github_url.scheme()))
            .context("build GitHub API URL")
    } else {
        Url::parse(&format!("{}://{hostport}/api/v3/", github_url.scheme()))
            .context("build GitHub Enterprise API URL")
    }
}

fn token_scope_path(segments: &[&str]) -> Result<String> {
    match segments {
        [org] => Ok(format!("orgs/{org}")),
        [first, second] if first.eq_ignore_ascii_case("enterprises") => {
            Ok(format!("enterprises/{second}"))
        }
        [owner, repo] => Ok(format!("repos/{owner}/{repo}")),
        _ => bail!("unsupported GitHub runner scope"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GitHubJitConfigRequest {
    pub name: String,
    pub runner_group_id: i64,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubJitConfigResponse {
    pub runner: GitHubJitRunner,
    pub encoded_jit_config: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubJitRunner {
    pub id: i64,
    pub name: String,
    pub os: String,
    pub status: String,
    pub busy: bool,
    pub labels: Vec<GitHubJitRunnerLabel>,
    #[serde(default)]
    pub runner_group_id: Option<i64>,
    #[serde(default)]
    pub ephemeral: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubJitRunnerLabel {
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RunnerGroup {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedJitConfig {
    pub settings: DecodedJitRunnerSettings,
    pub credentials: DecodedJitCredentials,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListedRunner {
    pub id: Option<i64>,
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub busy: Option<bool>,
    #[serde(default)]
    pub labels: Vec<GitHubJitRunnerLabel>,
}

fn deser_bool_from_any<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("boolean or string boolean")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(v)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            match v {
                "true" | "True" | "TRUE" => Ok(true),
                "false" | "False" | "FALSE" => Ok(false),
                _ => Err(E::custom(format!("expected bool string, got: {v}"))),
            }
        }
    }
    d.deserialize_any(Visitor)
}

fn ws_host(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .unwrap_or("results-receiver.actions.githubusercontent.com")
}

fn deser_opt_i64_from_any<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Option<i64>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("integer, string integer, or null")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as i64))
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() {
                return Ok(None);
            }
            v.parse::<i64>().map(Some).map_err(E::custom)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            serde::de::Deserialize::deserialize(d)
        }
    }
    d.deserialize_any(Visitor)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DecodedJitRunnerSettings {
    #[serde(
        default,
        rename = "AgentId",
        alias = "agentId",
        alias = "agent_id",
        deserialize_with = "deser_opt_i64_from_any"
    )]
    pub agent_id: Option<i64>,
    #[serde(
        default,
        rename = "AgentName",
        alias = "agentName",
        alias = "agent_name"
    )]
    pub agent_name: Option<String>,
    #[serde(
        default,
        rename = "PoolId",
        alias = "poolId",
        alias = "pool_id",
        deserialize_with = "deser_opt_i64_from_any"
    )]
    pub pool_id: Option<i64>,
    #[serde(default, rename = "PoolName", alias = "poolName", alias = "pool_name")]
    pub pool_name: Option<String>,
    #[serde(
        default,
        rename = "ServerUrl",
        alias = "serverUrl",
        alias = "server_url"
    )]
    pub server_url: Option<String>,
    #[serde(
        default,
        rename = "ServerUrlV2",
        alias = "serverUrlV2",
        alias = "server_url_v2"
    )]
    pub server_url_v2: Option<String>,
    #[serde(
        default,
        rename = "GitHubUrl",
        alias = "gitHubUrl",
        alias = "github_url"
    )]
    pub github_url: Option<String>,
    #[serde(
        default,
        rename = "WorkFolder",
        alias = "workFolder",
        alias = "work_folder"
    )]
    pub work_folder: Option<String>,
    #[serde(
        default,
        rename = "UseV2Flow",
        alias = "useV2Flow",
        alias = "use_v2_flow",
        deserialize_with = "deser_bool_from_any"
    )]
    pub use_v2_flow: bool,
    #[serde(
        default,
        rename = "Ephemeral",
        alias = "ephemeral",
        deserialize_with = "deser_bool_from_any"
    )]
    pub ephemeral: bool,
    #[serde(
        default,
        rename = "DisableUpdate",
        alias = "disableUpdate",
        alias = "disable_update",
        deserialize_with = "deser_bool_from_any"
    )]
    pub disable_update: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DecodedJitCredentials {
    #[serde(rename = "Scheme", alias = "scheme")]
    pub scheme: String,
    #[serde(rename = "Data", alias = "data")]
    pub data: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OAuthJwtCredentials {
    pub client_id: String,
    pub authorization_url: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthJwtClaims {
    iss: String,
    sub: String,
    aud: String,
    jti: String,
    nbf: u64,
    exp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenResponse {
    #[serde(rename = "access_token")]
    pub access_token: Option<String>,
    #[serde(rename = "token_type")]
    pub token_type: Option<String>,
    #[serde(rename = "expires_in")]
    pub expires_in: Option<i64>,
    #[serde(rename = "error")]
    pub error: Option<String>,
    #[serde(rename = "error_description")]
    pub error_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAccessToken {
    pub token: String,
    pub expires_in: Option<std::time::Duration>,
}

#[derive(Clone)]
pub struct OAuthClient {
    http: Client,
}

/// GitHub rejected JIT OAuth credentials because their runner registration
/// no longer exists. The daemon must discard the stored JIT configuration and
/// register again; retrying the same credentials can never recover.
#[derive(Debug, thiserror::Error)]
#[error("GitHub runner registration no longer exists: {0}")]
pub(crate) struct OAuthRegistrationNotFound(pub(crate) String);

fn oauth_registration_not_found(error: &str) -> bool {
    // Match actions/runner's MessageListener and BrokerMessageListener: the
    // service's invalid_client code means the runner registration was deleted.
    // Do not couple recovery to mutable, localized description prose.
    error.eq_ignore_ascii_case("invalid_client")
}

impl OAuthClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build OAuth HTTP client")?;
        Ok(Self { http })
    }

    pub async fn exchange_client_credentials(
        &self,
        credentials: &OAuthJwtCredentials,
    ) -> Result<OAuthAccessToken> {
        let assertion = build_client_assertion(credentials)?;
        // Build URL-encoded form body for curl --data
        let body: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair(
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            )
            .append_pair("client_assertion", &assertion)
            .finish();
        let url = credentials.authorization_url.clone();
        let ua = RUNNER_USER_AGENT.to_string();

        let cfg = format!(
            "header = \"User-Agent: {ua}\"\n\
             header = \"Accept: application/json\"\n\
             header = \"Content-Type: application/x-www-form-urlencoded\"\n\
             request = POST\n\
             connect-timeout = {GITHUB_CURL_CONNECT_TIMEOUT_SECS}\n\
             max-time = {GITHUB_CURL_MAX_TIME_SECS}\n\
             silent\n\
             write-out = \"\\n%{{http_code}}\"\n"
        );
        let (output, _) =
            run_private_curl("velnor-oauth", &cfg, Some(body.as_bytes()), &url, false).await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (text, status_str) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
        let status: u16 = status_str.trim().parse().unwrap_or(0);
        let ok = (200..300).contains(&status);
        if !ok && status != 400 {
            return Err(github_api_error("OAuth token request", status, text));
        }

        let token_response: OAuthTokenResponse =
            serde_json::from_str(text.trim()).context("parse OAuth token response")?;

        if let Some(error) = token_response.error {
            let description = token_response.error_description.unwrap_or_default();
            if oauth_registration_not_found(&error) {
                return Err(OAuthRegistrationNotFound(description).into());
            }
            bail!(
                "OAuth token request failed: error={error}, description={}",
                description
            );
        }

        let token = token_response
            .access_token
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("OAuth token response missing access_token"))?;
        Ok(OAuthAccessToken {
            token,
            expires_in: token_response
                .expires_in
                .and_then(|seconds| u64::try_from(seconds).ok())
                .filter(|seconds| *seconds > 0)
                .map(std::time::Duration::from_secs),
        })
    }
}

fn build_client_assertion(credentials: &OAuthJwtCredentials) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs();
    let claims = OAuthJwtClaims {
        iss: credentials.client_id.clone(),
        sub: credentials.client_id.clone(),
        aud: credentials.authorization_url.clone(),
        jti: Uuid::new_v4().to_string(),
        // Backdate the whole 300s validity window by 120s: a host clock
        // running ahead of GitHub's (skew) must not reject every OAuth
        // exchange while other API calls still work. The assertion is used
        // immediately, so trading future validity (180s left) for skew
        // tolerance is free; the 300s total lifetime stays within GitHub's
        // accepted assertion lifetime.
        nbf: now.saturating_sub(120),
        exp: now.saturating_sub(120) + 300,
    };
    let header = Header::new(Algorithm::RS256);
    let key = EncodingKey::from_rsa_pem(credentials.private_key_pem.as_bytes())
        .context("load runner RSA private key")?;

    encode(&header, &claims, &key).context("sign OAuth client assertion")
}

#[derive(Debug, Clone)]
pub struct RunnerKeyPair {
    pub private_key_pem: String,
    pub public_key: TaskAgentPublicKey,
}

impl RunnerKeyPair {
    pub fn generate() -> Result<Self> {
        let private_key =
            RsaPrivateKey::new(&mut OsRng, 2048).context("generate runner RSA key")?;
        let public_key = private_key.to_public_key();
        let private_key_pem = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .context("encode runner private key")?
            .to_string();

        Ok(Self {
            private_key_pem,
            public_key: TaskAgentPublicKey::from_public_key(&public_key),
        })
    }
}

#[derive(Clone)]
pub struct RegistrationClient {
    http: Client,
}

fn jit_curl_config(user_agent: &str, pat: &str) -> String {
    format!(
        "header = \"User-Agent: {user_agent}\"\n\
         header = \"Authorization: Bearer {pat}\"\n\
         header = \"Accept: application/vnd.github+json\"\n\
         header = \"X-GitHub-Api-Version: 2022-11-28\"\n\
         header = \"Content-Type: application/json\"\n\
         request = POST\n\
         connect-timeout = {JIT_CURL_CONNECT_TIMEOUT_SECONDS}\n\
         max-time = {JIT_CURL_MAX_TIME_SECONDS}\n\
         location\n\
         silent\n\
         write-out = \"\\n%{{http_code}}\"\n"
    )
}

impl RegistrationClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .use_native_tls()
            .tcp_keepalive(None)
            .connection_verbose(false)
            .timeout(Duration::from_secs(10))
            .build()
            .context("build GitHub runner HTTP client")?;
        Ok(Self { http })
    }

    pub async fn generate_jit_config(
        &self,
        scope: &GitHubScope,
        pat: &str,
        request: &GitHubJitConfigRequest,
    ) -> Result<GitHubJitConfigResponse> {
        // Use curl for runner registration. GitHub's infrastructure applies
        // TLS-fingerprint-based throttling to reqwest/hyper connections while
        // libcurl (LibreSSL) succeeds reliably. The subprocess is time-bounded
        // because registration runs inside a watchdog-supervised controller.
        let url = scope.jit_config_url.to_string();
        let body = serde_json::to_string(request).context("serialize JIT config request")?;
        let pat = pat.to_string();
        let ua = RUNNER_USER_AGENT.to_string();

        let mut last_err = anyhow::anyhow!("no attempts made");
        let mut retry_delay = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let backoff = retry_delay
                    .take()
                    .unwrap_or_else(|| std::time::Duration::from_secs(u64::from(attempt) * 5))
                    .min(GITHUB_RETRY_SLEEP_MAX);
                eprintln!(
                    "JIT config error (attempt {}/3), retrying in {}s",
                    attempt,
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
            }
            let url2 = url.clone();
            let body2 = body.clone();
            let pat2 = pat.clone();
            let ua2 = ua.clone();
            let result = tokio::task::spawn_blocking(move || {
                use std::os::unix::fs::OpenOptionsExt;
                let tmp = std::env::temp_dir();
                let cfg_path = tmp.join(format!("velnor-jit-{}.cfg", uuid::Uuid::new_v4()));
                let body_path = tmp.join(format!("velnor-jit-{}.body", uuid::Uuid::new_v4()));
                let headers_path = tmp.join(format!("velnor-jit-{}.headers", uuid::Uuid::new_v4()));
                let cfg = jit_curl_config(&ua2, &pat2);
                let write_0600 = |p: &std::path::Path, c: &[u8]| -> std::io::Result<()> {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(p)?;
                    f.write_all(c)
                };
                write_0600(&cfg_path, cfg.as_bytes())?;
                if let Err(e) = write_0600(&body_path, body2.as_bytes()) {
                    let _ = std::fs::remove_file(&cfg_path);
                    return Err(e);
                }
                let out = std::process::Command::new("curl")
                    .arg("--config")
                    .arg(&cfg_path)
                    .arg("--dump-header")
                    .arg(&headers_path)
                    .arg("--data")
                    .arg(format!("@{}", body_path.display()))
                    .arg(&url2)
                    .output();
                let _ = std::fs::remove_file(&cfg_path);
                let _ = std::fs::remove_file(&body_path);
                let headers = std::fs::read(&headers_path).unwrap_or_default();
                let _ = std::fs::remove_file(&headers_path);
                out.map(|output| (output, headers))
            })
            .await
            .context("spawn_blocking curl")?;

            match result {
                Err(e) => {
                    last_err = e.context("send JIT runner config request");
                    self.cleanup_named_jit_orphans(scope, &pat, &request.name)
                        .await;
                }
                Ok((output, headers)) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let (json_part, status_str) =
                        stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
                    let status: u16 = status_str.trim().parse().unwrap_or(0);
                    if status == 201 {
                        return serde_json::from_str::<GitHubJitConfigResponse>(json_part.trim())
                            .context("parse JIT runner config response");
                    }
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let hint = parse_github_retry_headers(&headers);
                    last_err = github_api_error_with_retry(
                        "JIT runner config request",
                        status,
                        format!("{json_part}, stderr={stderr}"),
                        hint,
                    );
                    // Permission and quota failures cannot recover during
                    // this attempt. Return immediately so controller pacing
                    // owns the retry schedule; never sleep for an hour on a
                    // GitHub reset header.
                    if matches!(status, 403 | 429) || status == 409 {
                        return Err(last_err);
                    }
                    self.cleanup_named_jit_orphans(scope, &pat, &request.name)
                        .await;
                    retry_delay = hint.delay(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    );
                }
            }
        }
        Err(last_err)
    }

    /// List every runner registration in scope. Paginated (100/page): the
    /// default 30-item page silently truncates fleets — doctor counts and
    /// orphan-cleanup-by-name must see ALL runners or they misjudge state.
    pub async fn list_runners(&self, scope: &GitHubScope, pat: &str) -> Result<Vec<ListedRunner>> {
        #[derive(Deserialize)]
        struct Page {
            total_count: Option<u64>,
            runners: Vec<ListedRunner>,
        }
        let base = scope.runners_url()?;
        let mut all = Vec::new();
        let mut page_number = 1u32;
        loop {
            let mut url = base.clone();
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page_number.to_string());
            let response = self
                .http
                .get(url)
                .bearer_auth(pat)
                .header(USER_AGENT, RUNNER_USER_AGENT)
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .await
                .context("send list runners request")?;
            let status = response.status();
            if !status.is_success() {
                let hint = github_retry_hint_from_header_map(response.headers());
                let body = response.text().await.unwrap_or_default();
                return Err(github_api_error_with_retry(
                    "list runners request",
                    status.as_u16(),
                    body,
                    hint,
                ));
            }
            let page: Page = response
                .json()
                .await
                .context("parse list runners response")?;
            let fetched = page.runners.len();
            all.extend(page.runners);
            let total = page.total_count.unwrap_or(all.len() as u64);
            if fetched < 100 || all.len() as u64 >= total {
                return Ok(all);
            }
            page_number += 1;
        }
    }

    async fn cleanup_named_jit_orphans(&self, scope: &GitHubScope, pat: &str, agent_name: &str) {
        let cleanup = async {
            let runners = self.list_runners(scope, pat).await?;
            for runner in runners
                .iter()
                .filter(|runner| runner.name.as_deref() == Some(agent_name))
            {
                let Some(id) = runner.id else {
                    continue;
                };
                match self.delete_runner(scope, pat, id).await {
                    Ok(()) => eprintln!(
                        "deleted uncertain JIT runner '{agent_name}' id {id} before retry"
                    ),
                    Err(error) if error.downcast_ref::<RunnerBusyConflict>().is_some() => {
                        eprintln!(
                            "kept busy JIT runner '{agent_name}' id {id} during retry cleanup"
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(Duration::from_secs(GITHUB_CURL_MAX_TIME_SECS), cleanup).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!("JIT retry orphan cleanup failed for '{agent_name}': {error:#}");
            }
            Err(_) => {
                eprintln!("JIT retry orphan cleanup timed out for '{agent_name}'");
            }
        }
    }

    pub async fn list_runner_groups(
        &self,
        scope: &GitHubScope,
        pat: &str,
    ) -> Result<Vec<RunnerGroup>> {
        #[derive(Deserialize)]
        struct Response {
            runner_groups: Vec<RunnerGroup>,
        }
        let response = self
            .http
            .get(scope.runner_groups_url()?)
            .bearer_auth(pat)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("send list runner groups request")?;
        let status = response.status();
        if !status.is_success() {
            let hint = github_retry_hint_from_header_map(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(github_api_error_with_retry(
                "list runner groups request",
                status.as_u16(),
                body,
                hint,
            ));
        }
        response
            .json::<Response>()
            .await
            .map(|response| response.runner_groups)
            .context("parse list runner groups response")
    }

    /// Queued (unassigned) jobs in this org/repo whose labels wait on Velnor.
    pub async fn list_queued_jobs(
        &self,
        scope: &GitHubScope,
        pat: &str,
    ) -> Result<Vec<ListedWorkflowJob>> {
        let repositories = self.list_scope_repositories(scope, pat).await?;
        let mut jobs = Vec::new();
        for repository in repositories {
            let runs = match self
                .list_queued_workflow_runs(scope, pat, &repository)
                .await
            {
                Ok(runs) => runs,
                Err(error) => {
                    eprintln!("list queued runs for {repository} failed: {error:#}");
                    continue;
                }
            };
            for run_id in runs {
                match self
                    .list_workflow_run_jobs(scope, pat, &repository, run_id)
                    .await
                {
                    Ok(run_jobs) => jobs.extend(run_jobs),
                    Err(error) => {
                        eprintln!("list jobs for {repository} run {run_id} failed: {error:#}");
                    }
                }
            }
        }
        Ok(jobs)
    }

    async fn list_scope_repositories(&self, scope: &GitHubScope, pat: &str) -> Result<Vec<String>> {
        if let Some((owner, repo)) = scope.repo_full_name() {
            return Ok(vec![format!("{owner}/{repo}")]);
        }
        let Some(org) = scope.org_login() else {
            return Ok(Vec::new());
        };
        #[derive(Deserialize)]
        struct Repo {
            full_name: Option<String>,
        }
        let base = scope.org_repos_url()?;
        let mut all = Vec::new();
        let mut page_number = 1u32;
        loop {
            let mut url = base.clone();
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page_number.to_string())
                .append_pair("type", "all");
            let response = self
                .http
                .get(url)
                .bearer_auth(pat)
                .header(USER_AGENT, RUNNER_USER_AGENT)
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .await
                .with_context(|| format!("list repositories for org {org}"))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(github_api_error(
                    "list org repositories",
                    status.as_u16(),
                    body,
                ));
            }
            let page: Vec<Repo> = response.json().await.context("parse org repositories")?;
            let fetched = page.len();
            all.extend(page.into_iter().filter_map(|repo| repo.full_name));
            if fetched < 100 {
                return Ok(all);
            }
            page_number += 1;
        }
    }

    async fn list_queued_workflow_runs(
        &self,
        scope: &GitHubScope,
        pat: &str,
        repository: &str,
    ) -> Result<Vec<u64>> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
            id: u64,
        }
        let mut url = scope.repo_queued_runs_url(repository)?;
        url.query_pairs_mut()
            .append_pair("status", "queued")
            .append_pair("per_page", "100");
        let response = self
            .http
            .get(url)
            .bearer_auth(pat)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("list queued runs for {repository}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(github_api_error(
                "list queued workflow runs",
                status.as_u16(),
                body,
            ));
        }
        let runs: Runs = response
            .json()
            .await
            .context("parse queued workflow runs")?;
        Ok(runs.workflow_runs.into_iter().map(|run| run.id).collect())
    }

    async fn list_workflow_run_jobs(
        &self,
        scope: &GitHubScope,
        pat: &str,
        repository: &str,
        run_id: u64,
    ) -> Result<Vec<ListedWorkflowJob>> {
        #[derive(Deserialize)]
        struct Jobs {
            jobs: Vec<ListedWorkflowJob>,
        }
        let url = scope
            .api_base_url
            .join(&format!("repos/{repository}/actions/runs/{run_id}/jobs"))
            .context("build workflow run jobs URL")?;
        let response = self
            .http
            .get(url)
            .bearer_auth(pat)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("list jobs for {repository} run {run_id}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(github_api_error(
                "list workflow run jobs",
                status.as_u16(),
                body,
            ));
        }
        let jobs: Jobs = response.json().await.context("parse workflow run jobs")?;
        Ok(jobs.jobs)
    }

    pub async fn cancel_workflow_run(
        &self,
        scope: &GitHubScope,
        pat: &str,
        repository: &str,
        run_id: u64,
    ) -> Result<()> {
        let url = scope.workflow_run_cancel_url(repository, run_id)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(pat)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("cancel workflow run {repository}/{run_id}"))?;
        let status = response.status().as_u16();
        if classify_workflow_cancel(status) {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        Err(github_api_error("cancel workflow run", status, body))
    }

    /// Look up one runner registration by id. `Ok(None)` means GitHub no
    /// longer knows the runner (404). Curl is the production default because
    /// reqwest/hyper has drawn TLS-fingerprint throttling under idle-slot load;
    /// `VELNOR_GITHUB_HTTP_TRANSPORT=native` enables the pooled canary path.
    pub async fn get_runner(
        &self,
        scope: &GitHubScope,
        pat: &str,
        runner_id: i64,
    ) -> Result<Option<ListedRunner>> {
        let url = scope.runner_url(runner_id)?;
        let (status, body) = github_json_request("GET", url.as_str(), pat, None, 30).await?;
        parse_runner_lookup(status, &body)
    }

    pub async fn delete_runner(
        &self,
        scope: &GitHubScope,
        pat: &str,
        runner_id: i64,
    ) -> Result<()> {
        let url = scope.runner_url(runner_id)?.to_string();
        let cfg = format!(
            "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
             header = \"Authorization: Bearer {pat}\"\n\
             header = \"Accept: application/vnd.github+json\"\n\
             header = \"X-GitHub-Api-Version: 2022-11-28\"\n\
             request = DELETE\n\
             connect-timeout = {GITHUB_CURL_CONNECT_TIMEOUT_SECS}\n\
             max-time = {GITHUB_CURL_MAX_TIME_SECS}\n\
             silent\n\
             write-out = \"\\n%{{http_code}}\"\n"
        );
        let (output, headers) = run_private_curl("velnor-del", &cfg, None, &url, true).await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (body, status_str) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
        let status: u16 = status_str.trim().parse().unwrap_or(0);
        match classify_runner_delete(status, body) {
            Some(RunnerDeleteOutcome::Gone) => Ok(()),
            Some(RunnerDeleteOutcome::BusyConflict) => {
                Err(RunnerBusyConflict(body.trim().to_string()).into())
            }
            None => Err(github_api_error_with_retry(
                "delete runner request",
                status,
                body,
                parse_github_retry_headers(&headers),
            )),
        }
    }
}

/// Transport selector for GitHub JSON requests.
///
/// Curl remains the production default because GitHub has historically
/// throttled the native TLS fingerprint under concurrent Velnor load. The
/// native path is an explicit canary so it can be measured and rolled back by
/// removing one daemon environment value, without changing workflow behavior.
pub const GITHUB_HTTP_TRANSPORT_ENV: &str = "VELNOR_GITHUB_HTTP_TRANSPORT";

pub fn github_http_transport() -> &'static str {
    match std::env::var(GITHUB_HTTP_TRANSPORT_ENV)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "native" | "reqwest" => "native",
        _ => "curl",
    }
}

/// Make an authenticated JSON request using the selected GitHub transport.
/// Returns `(http_status_code, response_body_string)`.
pub async fn github_json_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String)> {
    if github_http_transport() == "native" {
        native_json_request(method, url, bearer_token, json_body, max_time_secs).await
    } else {
        curl_json_request(method, url, bearer_token, json_body, max_time_secs).await
    }
}

/// Like [`github_json_request`] but also returns the rate-limit telemetry
/// from the response headers. Periodic callers (the controller probe) use it
/// to pace themselves against the shared PAT budget instead of discovering
/// exhaustion one 403 at a time.
pub async fn github_json_request_with_rate_limit(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String, GitHubRateLimitStatus)> {
    if github_http_transport() == "native" {
        native_json_request_with_rate_limit(method, url, bearer_token, json_body, max_time_secs)
            .await
    } else {
        curl_json_request_with_rate_limit(method, url, bearer_token, json_body, max_time_secs).await
    }
}

fn native_http_client() -> Result<Client> {
    static CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .use_native_tls()
            .tcp_keepalive(None)
            .connection_verbose(false)
            .build()
            .map_err(|error| format!("build native GitHub HTTP client: {error}"))
    }) {
        Ok(client) => Ok(client.clone()),
        Err(error) => bail!("{error}"),
    }
}

async fn native_json_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String)> {
    let method_name = method.to_string();
    let method = Method::from_bytes(method.as_bytes())
        .with_context(|| format!("parse GitHub HTTP method '{method}'"))?;
    let client = native_http_client()?;
    let mut request = client
        .request(method, url)
        .bearer_auth(bearer_token)
        .header(USER_AGENT, RUNNER_USER_AGENT)
        .header(ACCEPT, "application/json")
        .timeout(std::time::Duration::from_secs(max_time_secs));
    if let Some(body) = json_body {
        request = request
            .header("Content-Type", "application/json")
            .body(body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("send native GitHub request {method_name} {url}"))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .context("read native GitHub response body")?;
    Ok((status, body))
}

/// Native transport variant that also reports rate-limit telemetry.
async fn native_json_request_with_rate_limit(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String, GitHubRateLimitStatus)> {
    let method_name = method.to_string();
    let method = Method::from_bytes(method.as_bytes())
        .with_context(|| format!("parse GitHub HTTP method '{method}'"))?;
    let client = native_http_client()?;
    let mut request = client
        .request(method, url)
        .bearer_auth(bearer_token)
        .header(USER_AGENT, RUNNER_USER_AGENT)
        .header(ACCEPT, "application/json")
        .timeout(std::time::Duration::from_secs(max_time_secs));
    if let Some(body) = json_body {
        request = request
            .header("Content-Type", "application/json")
            .body(body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("send native GitHub request {method_name} {url}"))?;
    let status = response.status().as_u16();
    let rate_limit = github_retry_hint_from_header_map(response.headers()).into();
    let body = response
        .text()
        .await
        .context("read native GitHub response body")?;
    Ok((status, body, rate_limit))
}

/// Make an HTTP request via the curl subprocess. GitHub's infrastructure
/// applies TLS-fingerprint-based throttling to reqwest/hyper connections;
/// curl (LibreSSL) is the proven production fallback.
/// Returns `(http_status_code, response_body_string)`.
///
/// The Authorization header and request body are written to mode-0600 temp files
/// and passed via `--config` / `--data @file` so they do not appear on argv
/// (which is visible in `ps aux` and audit logs).
async fn curl_json_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String)> {
    let (status, body, _) =
        curl_json_request_impl(method, url, bearer_token, json_body, max_time_secs, false).await?;
    Ok((status, body))
}

/// Curl transport variant that also reports rate-limit telemetry via a
/// `--dump-header` temp file (parsed and removed before returning).
async fn curl_json_request_with_rate_limit(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String, GitHubRateLimitStatus)> {
    let (status, body, hint) =
        curl_json_request_impl(method, url, bearer_token, json_body, max_time_secs, true).await?;
    Ok((status, body, hint.unwrap_or_default().into()))
}

async fn curl_json_request_impl(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
    capture_headers: bool,
) -> Result<(u16, String, Option<GitHubRetryHint>)> {
    let mut cfg = format!(
        "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
         header = \"Authorization: Bearer {bearer_token}\"\n\
         header = \"Accept: application/json\"\n\
         max-time = {max_time_secs}\n\
         connect-timeout = {GITHUB_CURL_CONNECT_TIMEOUT_SECS}\n\
         request = {method}\n\
         location\n\
         silent\n\
         write-out = \"\\n%{{http_code}}\"\n"
    );
    if json_body.is_some() {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
    }
    let (output, headers) = run_private_curl(
        "velnor-curl",
        &cfg,
        json_body.as_deref().map(str::as_bytes),
        url,
        capture_headers,
    )
    .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (body, status_str) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
    let status: u16 = status_str.trim().parse().unwrap_or(0);
    Ok((
        status,
        body.to_string(),
        capture_headers.then(|| parse_github_retry_headers(&headers)),
    ))
}

pub fn decode_jit_config(encoded_jit_config: &str) -> Result<DecodedJitConfig> {
    let decoded = STANDARD
        .decode(encoded_jit_config)
        .context("decode encoded_jit_config")?;
    let decoded = String::from_utf8(decoded).context("decode encoded_jit_config UTF-8")?;
    let file_map: BTreeMap<String, String> =
        serde_json::from_str(&decoded).context("parse encoded_jit_config file map")?;

    let settings = decode_jit_file(&file_map, ".runner")?;
    let credentials = decode_jit_file(&file_map, ".credentials")?;
    let rsa_params = decode_jit_file_bytes(&file_map, ".credentials_rsaparams")?;
    let private_key_pem = rsa_parameters_json_to_pem(&rsa_params)?;

    Ok(DecodedJitConfig {
        settings,
        credentials,
        private_key_pem,
    })
}

fn decode_jit_file<T>(file_map: &BTreeMap<String, String>, name: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = decode_jit_file_bytes(file_map, name)?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse JIT config file {name}"))
}

fn decode_jit_file_bytes(file_map: &BTreeMap<String, String>, name: &str) -> Result<Vec<u8>> {
    let encoded = file_map
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("encoded_jit_config missing {name}"))?;
    STANDARD
        .decode(encoded)
        .with_context(|| format!("decode JIT config file {name}"))
}

fn rsa_parameters_json_to_pem(json_bytes: &[u8]) -> Result<String> {
    let params: RsaParametersJson =
        serde_json::from_slice(json_bytes).context("parse JIT RSA parameters")?;
    let key = RsaPrivateKey::from_components(
        BigUint::from_bytes_be(&params.modulus.decode()?),
        BigUint::from_bytes_be(&params.exponent.decode()?),
        BigUint::from_bytes_be(&params.d.decode()?),
        vec![
            BigUint::from_bytes_be(&params.p.decode()?),
            BigUint::from_bytes_be(&params.q.decode()?),
        ],
    )
    .context("build RSA private key from JIT parameters")?;
    key.to_pkcs8_pem(LineEnding::LF)
        .context("encode JIT RSA private key")
        .map(|pem| pem.to_string())
}

#[derive(Debug, Deserialize)]
struct RsaParametersJson {
    #[serde(rename = "d")]
    d: JsonBytes,
    #[serde(rename = "exponent")]
    exponent: JsonBytes,
    #[serde(rename = "modulus")]
    modulus: JsonBytes,
    #[serde(rename = "p")]
    p: JsonBytes,
    #[serde(rename = "q")]
    q: JsonBytes,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonBytes {
    Base64(String),
    Array(Vec<u8>),
}

impl JsonBytes {
    fn decode(&self) -> Result<Vec<u8>> {
        match self {
            JsonBytes::Base64(value) => STANDARD.decode(value).context("decode RSA parameter"),
            JsonBytes::Array(value) => Ok(value.clone()),
        }
    }
}

#[derive(Clone)]
pub struct DistributedTaskClient {
    http: Client,
    server_root_url: Url,
    base_url: Url,
    bearer_token: String,
}

#[derive(Clone)]
pub struct BrokerClient {
    http: Client,
    base_url: Url,
    bearer_token: String,
}

/// One broker long-poll outcome: HTTP status (for forensic logs) plus the
/// decoded message, when any.
#[derive(Debug)]
pub struct BrokerPoll {
    pub status: u16,
    pub message: Option<TaskAgentMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerPollClass {
    /// Healthy long-poll cycle with no work (HTTP 204, or 2xx with empty body).
    Empty,
    /// 2xx with a message body to decode.
    Message,
    /// Transport failure (curl could not produce a status) or non-2xx. An
    /// expired/unauthorized/deleted session typically answers 401/403/404 with
    /// an EMPTY body — this MUST classify as an error, never as "no message",
    /// or an idle slot turns into a zombie that polls forever while GitHub's
    /// scheduler has already dropped the runner (2026-06-11 fleet incident).
    Error,
}

/// Recovery-relevant class for a failed broker poll. The HTTP status remains
/// available on `BrokerPoll`; this projection prevents callers from treating
/// a dead session, auth failure, quota pressure, and a transient outage as the
/// same idle retry condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerPollErrorClass {
    Authentication,
    Forbidden,
    MissingSession,
    Conflict,
    RateLimited,
    Client,
    Server,
    Transport,
}

#[must_use]
pub fn classify_broker_poll_error(status: u16) -> BrokerPollErrorClass {
    match status {
        401 => BrokerPollErrorClass::Authentication,
        403 => BrokerPollErrorClass::Forbidden,
        404 => BrokerPollErrorClass::MissingSession,
        409 => BrokerPollErrorClass::Conflict,
        429 => BrokerPollErrorClass::RateLimited,
        400..=499 => BrokerPollErrorClass::Client,
        500..=599 => BrokerPollErrorClass::Server,
        0 => BrokerPollErrorClass::Transport,
        _ => BrokerPollErrorClass::Transport,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationErrorClass {
    Quota,
    Permission,
    Missing,
    Conflict,
    Transient,
    Transport,
    Client,
}

#[must_use]
pub fn classify_registration_error(
    status: u16,
    remaining: Option<u64>,
    retry_after: Option<u64>,
) -> RegistrationErrorClass {
    match status {
        0 => RegistrationErrorClass::Transport,
        401 => RegistrationErrorClass::Permission,
        403 if remaining == Some(0) || retry_after.is_some() => RegistrationErrorClass::Quota,
        403 => RegistrationErrorClass::Permission,
        404 => RegistrationErrorClass::Missing,
        409 => RegistrationErrorClass::Conflict,
        429 => RegistrationErrorClass::Quota,
        400..=499 => RegistrationErrorClass::Client,
        _ => RegistrationErrorClass::Transient,
    }
}

pub fn classify_broker_poll(http_status: u16, body: &str) -> BrokerPollClass {
    if http_status == 204 {
        return BrokerPollClass::Empty;
    }
    if (200..300).contains(&http_status) {
        if body.trim().is_empty() {
            return BrokerPollClass::Empty;
        }
        return BrokerPollClass::Message;
    }
    BrokerPollClass::Error
}

/// Completion must retry transport failures, 5xx, and curl status-0; other
/// 4xx (auth, validation, conflict) will not change on retry.
pub fn is_retriable_completion_status(status: u16) -> bool {
    !(400..500).contains(&status) || status == 408 || status == 429
}

/// Decode a GET /actions/runners/{id} response: `Ok(None)` only on a definite
/// 404 (registration gone); other failures are errors so transient API trouble
/// is never mistaken for a deleted runner.
pub fn parse_runner_lookup(status: u16, body: &str) -> Result<Option<ListedRunner>> {
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(github_api_error("runner lookup", status, body.trim()));
    }
    serde_json::from_str(body.trim())
        .map(Some)
        .context("parse runner lookup response")
}

impl BrokerClient {
    pub fn new(server_url_v2: &str, bearer_token: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build broker HTTP client")?;
        Ok(Self {
            http,
            base_url: slash_url(server_url_v2)?,
            bearer_token: bearer_token.into(),
        })
    }

    /// Current broker base URL (post-migration aware), for rebuilding the
    /// client with refreshed credentials.
    pub fn base_url_str(&self) -> String {
        self.base_url.to_string()
    }

    pub async fn create_session(&self, session: &TaskAgentSession) -> Result<TaskAgentSession> {
        let url = broker_session_url(&self.base_url)?;
        let body = serde_json::to_string(session).context("serialize session")?;
        let (status, text) =
            github_json_request("POST", url.as_str(), &self.bearer_token, Some(body), 30).await?;
        if !(200..300).contains(&status) {
            return Err(github_api_error("create broker session", status, text));
        }
        serde_json::from_str(&text).context("parse create broker session response")
    }

    pub async fn delete_session(&self) -> Result<()> {
        let url = broker_session_url(&self.base_url)?;
        let (status, text) =
            github_json_request("DELETE", url.as_str(), &self.bearer_token, None, 30).await?;
        if status != 0 && !(200..300).contains(&status) {
            return Err(github_api_error("delete broker session", status, text));
        }
        Ok(())
    }

    pub async fn get_runner_message(
        &self,
        session_id: &str,
        status: RunnerStatus,
        disable_update: bool,
    ) -> Result<BrokerPoll> {
        let url = broker_message_url(&self.base_url, session_id, status, disable_update)?;
        let (http_status, text) =
            github_json_request("GET", url.as_str(), &self.bearer_token, None, 70).await?;
        match classify_broker_poll(http_status, &text) {
            BrokerPollClass::Empty => Ok(BrokerPoll {
                status: http_status,
                message: None,
            }),
            BrokerPollClass::Message => serde_json::from_str(text.trim())
                .map(|message| BrokerPoll {
                    status: http_status,
                    message: Some(message),
                })
                .context("parse get broker message response"),
            BrokerPollClass::Error => Err(github_api_error(
                "get broker message",
                http_status,
                text.trim(),
            )),
        }
    }

    pub async fn acknowledge_runner_request(
        &self,
        session_id: &str,
        runner_request_id: &str,
        status: RunnerStatus,
    ) -> Result<()> {
        let url = broker_acknowledge_url(&self.base_url, session_id, status)?;
        let body = json!({ "runnerRequestId": runner_request_id }).to_string();
        let (http_status, text) =
            github_json_request("POST", url.as_str(), &self.bearer_token, Some(body), 30).await?;
        if http_status != 0 && !(200..300).contains(&http_status) {
            return Err(github_api_error(
                "acknowledge broker runner request",
                http_status,
                text,
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct RunServiceClient {
    http: Client,
    bearer_token: String,
    #[cfg(test)]
    acquire_retry_delay_override: Option<Duration>,
}

#[derive(Debug)]
pub enum AcquireJobOutcome {
    Acquired(Value),
    Skipped {
        status: StatusCode,
        request_id: Option<String>,
        body: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum AcquireJobError {
    #[error("permanent run-service acquire failure: {0:#}")]
    Permanent(#[source] anyhow::Error),
    #[error("transient run-service acquire failure after retries: {0:#}")]
    Transient(#[source] anyhow::Error),
}

/// Whether an acquire failure is safe to absorb while the broker session
/// remains alive. Permanent protocol/configuration failures must tear down the
/// session so credentials or malformed payloads cannot spin forever.
pub(crate) fn is_transient_acquire_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<AcquireJobError>()
            .is_some_and(|error| matches!(error, AcquireJobError::Transient(_)))
    })
}

impl RunServiceClient {
    pub fn new(bearer_token: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build run-service HTTP client")?;
        Ok(Self {
            http,
            bearer_token: bearer_token.into(),
            #[cfg(test)]
            acquire_retry_delay_override: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_acquire_retry_delay_for_test(mut self, delay: Duration) -> Self {
        self.acquire_retry_delay_override = Some(delay);
        self
    }

    pub async fn acquire_job(
        &self,
        run_service_url: &str,
        job_message_id: &str,
        runner_os: &str,
        billing_owner_id: Option<&str>,
    ) -> Result<AcquireJobOutcome> {
        let url = run_service_acquire_job_url(run_service_url)
            .map_err(|error| anyhow::Error::from(AcquireJobError::Permanent(error)))?;
        let body = serde_json::to_string(&AcquireJobRequest {
            job_message_id,
            runner_os,
            billing_owner_id,
        })
        .context("serialize acquire job request")
        .map_err(|error| anyhow::Error::from(AcquireJobError::Permanent(error)))?;
        let mut attempt = 1;
        loop {
            let outcome = github_json_request(
                "POST",
                url.as_str(),
                &self.bearer_token,
                Some(body.clone()),
                30,
            )
            .await;

            let retry_error = match outcome {
                Ok((status, text)) => {
                    let status_code =
                        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    if is_non_retriable_acquire_status(status_code) {
                        return Ok(AcquireJobOutcome::Skipped {
                            status: status_code,
                            request_id: None,
                            body: text,
                        });
                    }
                    if !(200..300).contains(&status) {
                        Some(github_api_error("acquire run-service job", status, text))
                    } else {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => return Ok(AcquireJobOutcome::Acquired(value)),
                            Err(error) => {
                                return Err(AcquireJobError::Permanent(
                                    anyhow::Error::new(error)
                                        .context("parse acquire run-service job response"),
                                )
                                .into());
                            }
                        }
                    }
                }
                Err(error) => Some(error.context("acquire run-service job request")),
            };

            let Some(error) = retry_error else {
                unreachable!("successful acquire returns before retry handling");
            };
            if attempt >= RUN_SERVICE_ACQUIRE_MAX_ATTEMPTS {
                let error = if acquire_failure_is_transient(&error) {
                    AcquireJobError::Transient(error)
                } else {
                    AcquireJobError::Permanent(error)
                };
                return Err(error.into());
            }

            let delay = self.acquire_retry_delay(attempt);
            eprintln!(
                "acquire run-service job attempt {attempt}/{RUN_SERVICE_ACQUIRE_MAX_ATTEMPTS} failed ({error:#}); retrying in {}s",
                delay.as_secs()
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    fn acquire_retry_delay(&self, attempt: u32) -> Duration {
        #[cfg(test)]
        if let Some(delay) = self.acquire_retry_delay_override {
            return delay;
        }

        let span = RUN_SERVICE_ACQUIRE_RETRY_MAX_SECS - RUN_SERVICE_ACQUIRE_RETRY_MIN_SECS;
        let jitter = (std::process::id() as u64 + u64::from(attempt) * 7) % (span + 1);
        Duration::from_secs(RUN_SERVICE_ACQUIRE_RETRY_MIN_SECS + jitter)
    }

    pub async fn renew_job(
        &self,
        run_service_url: &str,
        plan_id: &str,
        job_id: &str,
    ) -> Result<RenewJobResponse> {
        let url = run_service_renew_job_url(run_service_url)?;
        let body = serde_json::to_string(&RenewJobRequest { plan_id, job_id })
            .context("serialize renew job request")?;
        let (status, text) =
            github_json_request("POST", url.as_str(), &self.bearer_token, Some(body), 30).await?;
        if !(200..300).contains(&status) {
            return Err(github_api_error("renew run-service job", status, text));
        }
        serde_json::from_str(&text).context("parse renew run-service job response")
    }

    /// Report the job result. Retried with backoff on transport failures and
    /// 5xx: a finished job's outcome must never be lost to one transient
    /// error — GitHub would mark the job "runner lost communication" while
    /// its side effects already happened.
    pub async fn complete_job(
        &self,
        run_service_url: &str,
        completion: RunServiceCompleteJob,
    ) -> Result<()> {
        const MAX_ATTEMPTS: u32 = 6;
        let url = run_service_complete_job_url(run_service_url)?;
        let body = serde_json::to_string(&completion).context("serialize complete job request")?;
        let mut attempt: u32 = 1;
        loop {
            let outcome = github_json_request(
                "POST",
                url.as_str(),
                &self.bearer_token,
                Some(body.clone()),
                30,
            )
            .await;
            let retriable = match &outcome {
                Ok((status, _)) if (200..300).contains(status) => return Ok(()),
                Ok((status, _)) => is_retriable_completion_status(*status),
                Err(_) => true,
            };
            if attempt >= MAX_ATTEMPTS || !retriable {
                return match outcome {
                    Ok((status, text)) => {
                        Err(github_api_error("complete run-service job", status, text))
                    }
                    Err(error) => Err(error).context("complete run-service job request"),
                };
            }
            let delay =
                std::time::Duration::from_secs(5u64.saturating_mul(1 << (attempt - 1)).min(60));
            match &outcome {
                Ok((status, _)) => eprintln!(
                    "complete job attempt {attempt}/{MAX_ATTEMPTS} failed (status={status}); retrying in {}s",
                    delay.as_secs()
                ),
                Err(error) => eprintln!(
                    "complete job attempt {attempt}/{MAX_ATTEMPTS} failed ({error:#}); retrying in {}s",
                    delay.as_secs()
                ),
            }
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }
}

fn acquire_failure_is_transient(error: &anyhow::Error) -> bool {
    let Some(api_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
    else {
        // Native request failures expose a reqwest error; curl failures that
        // cannot produce an HTTP status expose an I/O error. Filesystem and
        // executable errors are local faults and must not retain a session
        // forever as if GitHub were temporarily unavailable.
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|error| error.is_timeout() || error.is_connect())
        }) {
            return true;
        }
        return error.chain().any(|cause| {
            cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::Interrupted
                )
            })
        });
    };

    matches!(api_error.status, 0 | 408 | 429 | 500..=599)
}

impl DistributedTaskClient {
    pub fn new(server_url: &str, bearer_token: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build distributed task HTTP client")?;
        let server_root_url = server_root_url(server_url)?;
        Ok(Self {
            http,
            server_root_url,
            base_url: distributed_task_base_url(server_url)?,
            bearer_token: bearer_token.into(),
        })
    }

    pub async fn get_agent_pools(&self, pool_name: Option<&str>) -> Result<Vec<TaskAgentPool>> {
        let mut url = self.base_url.join("pools")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("api-version", "5.1-preview.1");
            if let Some(pool_name) = pool_name {
                query.append_pair("poolName", pool_name);
            }
        }

        self.get_list(url, "get agent pools").await
    }

    pub async fn get_agents(&self, pool_id: i64, agent_name: &str) -> Result<Vec<TaskAgent>> {
        let mut url = self.base_url.join(&format!("pools/{pool_id}/agents"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("api-version", "6.0-preview.2");
            query.append_pair("agentName", agent_name);
        }

        self.get_list(url, "get agents").await
    }

    pub async fn add_agent(&self, pool_id: i64, agent: &TaskAgent) -> Result<TaskAgent> {
        let mut url = self.base_url.join(&format!("pools/{pool_id}/agents"))?;
        url.query_pairs_mut()
            .append_pair("api-version", "6.0-preview.2");

        self.send_agent("POST", url, agent, "add agent").await
    }

    pub async fn replace_agent(&self, pool_id: i64, agent: &TaskAgent) -> Result<TaskAgent> {
        let agent_id = agent.id.context("replace agent needs agent id")?;
        let mut url = self
            .base_url
            .join(&format!("pools/{pool_id}/agents/{agent_id}"))?;
        url.query_pairs_mut()
            .append_pair("api-version", "6.0-preview.2");

        self.send_agent("PUT", url, agent, "replace agent").await
    }

    pub async fn delete_agent(&self, pool_id: i64, agent_id: i64) -> Result<()> {
        let mut url = self
            .base_url
            .join(&format!("pools/{pool_id}/agents/{agent_id}"))?;
        url.query_pairs_mut()
            .append_pair("api-version", "6.0-preview.2");

        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send delete agent request")?;

        parse_empty_response(response, "delete agent").await
    }

    pub async fn create_session(
        &self,
        pool_id: i64,
        session: &TaskAgentSession,
    ) -> Result<TaskAgentSession> {
        let mut url = self.base_url.join(&format!("pools/{pool_id}/sessions"))?;
        url.query_pairs_mut()
            .append_pair("api-version", "5.1-preview.1");

        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(session)
            .send()
            .await
            .context("send create session request")?;

        parse_json_response(response, "create session").await
    }

    pub async fn delete_session(&self, pool_id: i64, session_id: &str) -> Result<()> {
        let mut url = self
            .base_url
            .join(&format!("pools/{pool_id}/sessions/{session_id}"))?;
        url.query_pairs_mut()
            .append_pair("api-version", "5.1-preview.1");

        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send delete session request")?;

        parse_empty_response(response, "delete session").await
    }

    pub async fn get_message(
        &self,
        pool_id: i64,
        session_id: &str,
        last_message_id: Option<i64>,
        status: RunnerStatus,
        disable_update: bool,
    ) -> Result<Option<TaskAgentMessage>> {
        let mut url = self.base_url.join(&format!("pools/{pool_id}/messages"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("api-version", "6.0-preview.1");
            query.append_pair("sessionId", session_id);
            if let Some(last_message_id) = last_message_id {
                query.append_pair("lastMessageId", &last_message_id.to_string());
            }
            query.append_pair("status", status.as_query_value());
            query.append_pair("runnerVersion", RUNNER_VERSION);
            query.append_pair("os", std::env::consts::OS);
            query.append_pair("architecture", std::env::consts::ARCH);
            query.append_pair(
                "disableUpdate",
                if disable_update { "true" } else { "false" },
            );
        }

        let response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send get message request")?;

        parse_optional_task_agent_message_response(response, "get message").await
    }

    pub async fn delete_message(
        &self,
        pool_id: i64,
        message_id: i64,
        session_id: &str,
    ) -> Result<()> {
        let mut url = self
            .base_url
            .join(&format!("pools/{pool_id}/messages/{message_id}"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("api-version", "5.1-preview.1");
            query.append_pair("sessionId", session_id);
        }

        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send delete message request")?;

        parse_empty_response(response, "delete message").await
    }

    pub async fn renew_agent_request(
        &self,
        pool_id: i64,
        request_id: i64,
        orchestration_id: Option<&str>,
    ) -> Result<TaskAgentJobRequest> {
        let body = TaskAgentJobRequest::renew(request_id);
        let mut headers = HeaderMap::new();
        if let Some(orchestration_id) = orchestration_id.filter(|value| !value.is_empty()) {
            headers.insert(
                HeaderName::from_static("x-vss-orchestrationid"),
                HeaderValue::from_str(orchestration_id).context("invalid orchestration id")?,
            );
        }

        self.patch_agent_request(pool_id, request_id, &body, headers, "renew agent request")
            .await
    }

    pub async fn finish_agent_request(
        &self,
        pool_id: i64,
        request_id: i64,
        finish_time_utc: impl Into<String>,
        result: TaskResult,
    ) -> Result<TaskAgentJobRequest> {
        let body = TaskAgentJobRequest::finish(request_id, finish_time_utc, result);

        self.patch_agent_request(
            pool_id,
            request_id,
            &body,
            HeaderMap::new(),
            "finish agent request",
        )
        .await
    }

    pub async fn raise_job_completed_event(
        &self,
        scope_identifier: &str,
        hub_name: &str,
        plan_id: &str,
        event: &JobCompletedEvent,
    ) -> Result<()> {
        let url = plan_events_url(&self.server_root_url, scope_identifier, hub_name, plan_id)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(event)
            .send()
            .await
            .context("send job completed event request")?;

        parse_empty_response(response, "raise job completed event").await
    }

    pub async fn update_timeline_records(
        &self,
        scope_identifier: &str,
        hub_name: &str,
        plan_id: &str,
        timeline_id: &str,
        records: Vec<TimelineRecord>,
    ) -> Result<Vec<TimelineRecord>> {
        let url = timeline_records_url(
            &self.server_root_url,
            scope_identifier,
            hub_name,
            plan_id,
            timeline_id,
        )?;
        let body = VssJsonCollectionWrapper { value: records };
        let response = self
            .http
            .request(Method::PATCH, url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("send update timeline records request")?;

        parse_json_response(response, "update timeline records").await
    }

    pub async fn append_timeline_record_feed(
        &self,
        scope_identifier: &str,
        hub_name: &str,
        plan_id: &str,
        timeline_id: &str,
        record_id: &str,
        feed: TimelineRecordFeedLines,
    ) -> Result<()> {
        let url = timeline_record_feed_url(
            &self.server_root_url,
            scope_identifier,
            hub_name,
            plan_id,
            timeline_id,
            record_id,
        )?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&feed)
            .send()
            .await
            .context("send append timeline record feed request")?;

        parse_empty_response(response, "append timeline record feed").await
    }

    async fn patch_agent_request(
        &self,
        pool_id: i64,
        request_id: i64,
        body: &TaskAgentJobRequest,
        headers: HeaderMap,
        action: &str,
    ) -> Result<TaskAgentJobRequest> {
        let url = agent_request_url(&self.base_url, pool_id, request_id)?;

        let response = self
            .http
            .request(Method::PATCH, url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .headers(headers)
            .json(body)
            .send()
            .await
            .with_context(|| format!("send {action} request"))?;

        parse_json_response(response, action).await
    }

    async fn get_json<T>(&self, url: Url, action: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .with_context(|| format!("send {action} request"))?;

        parse_json_response(response, action).await
    }

    async fn get_list<T>(&self, url: Url, action: &str) -> Result<Vec<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let value: Value = self.get_json(url, action).await?;
        parse_vss_list(value, action)
    }

    async fn send_agent(
        &self,
        method: &str,
        url: Url,
        agent: &TaskAgent,
        action: &str,
    ) -> Result<TaskAgent> {
        let request = match method {
            "POST" => self.http.post(url),
            "PUT" => self.http.put(url),
            _ => bail!("unsupported agent method {method}"),
        };

        let response = request
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(agent)
            .send()
            .await
            .with_context(|| format!("send {action} request"))?;

        parse_json_response(response, action).await
    }
}

#[derive(Debug, Clone, Serialize)]
struct VssJsonCollectionWrapper<T> {
    value: T,
}

fn server_root_url(server_url: &str) -> Result<Url> {
    slash_url(server_url)
}

fn slash_url(server_url: &str) -> Result<Url> {
    let mut root =
        Url::parse(server_url).with_context(|| format!("parse server URL '{server_url}'"))?;
    if !root.path().ends_with('/') {
        let path = format!("{}/", root.path());
        root.set_path(&path);
    }
    Ok(root)
}

fn broker_session_url(base_url: &Url) -> Result<Url> {
    base_url.join("session").context("build broker session URL")
}

fn broker_message_url(
    base_url: &Url,
    session_id: &str,
    status: RunnerStatus,
    disable_update: bool,
) -> Result<Url> {
    let mut url = base_url
        .join("message")
        .context("build broker message URL")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("sessionId", session_id);
        query.append_pair("status", status.as_query_value());
        query.append_pair("runnerVersion", RUNNER_VERSION);
        query.append_pair("os", std::env::consts::OS);
        query.append_pair("architecture", std::env::consts::ARCH);
        query.append_pair(
            "disableUpdate",
            if disable_update { "true" } else { "false" },
        );
    }
    Ok(url)
}

fn broker_acknowledge_url(base_url: &Url, session_id: &str, status: RunnerStatus) -> Result<Url> {
    let mut url = base_url
        .join("acknowledge")
        .context("build broker acknowledge URL")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("sessionId", session_id);
        query.append_pair("status", status.as_query_value());
        query.append_pair("runnerVersion", RUNNER_VERSION);
        query.append_pair("os", std::env::consts::OS);
        query.append_pair("architecture", std::env::consts::ARCH);
    }
    Ok(url)
}

fn run_service_acquire_job_url(run_service_url: &str) -> Result<Url> {
    slash_url(run_service_url)?
        .join("acquirejob")
        .context("build run-service acquire job URL")
}

fn run_service_renew_job_url(run_service_url: &str) -> Result<Url> {
    slash_url(run_service_url)?
        .join("renewjob")
        .context("build run-service renew job URL")
}

fn run_service_complete_job_url(run_service_url: &str) -> Result<Url> {
    slash_url(run_service_url)?
        .join("completejob")
        .context("build run-service complete job URL")
}

fn agent_request_url(base_url: &Url, pool_id: i64, request_id: i64) -> Result<Url> {
    let mut url = base_url.join(&format!("pools/{pool_id}/jobrequests/{request_id}"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("api-version", "5.1-preview.1");
        query.append_pair("lockToken", EMPTY_LOCK_TOKEN);
    }
    Ok(url)
}

fn timeline_records_url(
    server_root_url: &Url,
    scope_identifier: &str,
    hub_name: &str,
    plan_id: &str,
    timeline_id: &str,
) -> Result<Url> {
    let mut url = server_root_url.join(&format!(
        "{scope_identifier}/_apis/distributedtask/hubs/{hub_name}/plans/{plan_id}/timelines/{timeline_id}/records"
    ))?;
    url.query_pairs_mut()
        .append_pair("api-version", "5.1-preview.1");
    Ok(url)
}

fn plan_events_url(
    server_root_url: &Url,
    scope_identifier: &str,
    hub_name: &str,
    plan_id: &str,
) -> Result<Url> {
    let mut url = server_root_url.join(&format!(
        "{scope_identifier}/_apis/distributedtask/hubs/{hub_name}/plans/{plan_id}/events"
    ))?;
    url.query_pairs_mut()
        .append_pair("api-version", "5.1-preview.1");
    Ok(url)
}

fn timeline_record_feed_url(
    server_root_url: &Url,
    scope_identifier: &str,
    hub_name: &str,
    plan_id: &str,
    timeline_id: &str,
    record_id: &str,
) -> Result<Url> {
    let mut url = server_root_url.join(&format!(
        "{scope_identifier}/_apis/distributedtask/hubs/{hub_name}/plans/{plan_id}/timelines/{timeline_id}/records/{record_id}/feed"
    ))?;
    url.query_pairs_mut()
        .append_pair("api-version", "5.1-preview.1");
    Ok(url)
}

fn timeline_logs_url(
    server_root_url: &Url,
    scope_identifier: &str,
    hub_name: &str,
    plan_id: &str,
) -> Result<Url> {
    let mut url = server_root_url.join(&format!(
        "{scope_identifier}/_apis/distributedtask/hubs/{hub_name}/plans/{plan_id}/logs"
    ))?;
    url.query_pairs_mut()
        .append_pair("api-version", "5.1-preview.1");
    Ok(url)
}

fn parse_vss_list<T>(value: Value, action: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if value.is_array() {
        return serde_json::from_value(value).with_context(|| format!("parse {action} list"));
    }

    if let Some(items) = value.get("value") {
        return serde_json::from_value(items.clone())
            .with_context(|| format!("parse {action} value list"));
    }

    bail!("{action} response was not a list")
}

fn null_string_default<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

async fn parse_json_response<T>(response: reqwest::Response, action: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-github-request-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(github_api_error(
            action,
            status.as_u16(),
            format!(
                "request_id={}, body={}",
                request_id.unwrap_or_else(|| "unknown".to_string()),
                body
            ),
        ));
    }

    response
        .json::<T>()
        .await
        .with_context(|| format!("parse {action} response"))
}

async fn parse_acquire_job_response(response: reqwest::Response) -> Result<AcquireJobOutcome> {
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-github-request-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    if is_non_retriable_acquire_status(status) {
        let body = response.text().await.unwrap_or_default();
        return Ok(AcquireJobOutcome::Skipped {
            status,
            request_id,
            body,
        });
    }

    parse_json_response::<Value>(response, "acquire run-service job")
        .await
        .map(AcquireJobOutcome::Acquired)
}

fn is_non_retriable_acquire_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    )
}

async fn parse_optional_json_response<T>(
    response: reqwest::Response,
    action: &str,
) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }

    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("read {action} response"))?;

    if !status.is_success() {
        return Err(github_api_error(action, status.as_u16(), text));
    }

    if text.trim().is_empty() {
        return Ok(None);
    }

    serde_json::from_str::<T>(&text)
        .map(Some)
        .with_context(|| format!("parse {action} response"))
}

async fn parse_optional_task_agent_message_response(
    response: reqwest::Response,
    action: &str,
) -> Result<Option<TaskAgentMessage>> {
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }

    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("read {action} response"))?;

    if !status.is_success() {
        return Err(github_api_error(action, status.as_u16(), text));
    }

    if text.trim().is_empty() {
        return Ok(None);
    }

    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {action} response"))?;
    serde_json::from_value(value)
        .map(Some)
        .with_context(|| format!("parse {action} response"))
}

async fn parse_empty_response(response: reqwest::Response, action: &str) -> Result<()> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(github_api_error(action, status.as_u16(), body));
    }
    Ok(())
}

fn distributed_task_base_url(server_url: &str) -> Result<Url> {
    let mut root =
        Url::parse(server_url).with_context(|| format!("parse server URL '{server_url}'"))?;
    if !root.path().ends_with('/') {
        let path = format!("{}/", root.path());
        root.set_path(&path);
    }
    root.join("_apis/distributedtask/")
        .context("build distributed task API URL")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentPool {
    #[serde(rename = "id")]
    pub id: i64,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "isHosted")]
    pub is_hosted: bool,
    #[serde(default, rename = "isInternal")]
    pub is_internal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentSession {
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "ownerName")]
    pub owner_name: String,
    #[serde(default, rename = "agent")]
    pub agent: TaskAgentReference,
    #[serde(default, rename = "useFipsEncryption")]
    pub use_fips_encryption: bool,
    #[serde(rename = "encryptionKey", skip_serializing_if = "Option::is_none")]
    pub encryption_key: Option<TaskAgentSessionKey>,
}

impl TaskAgentSession {
    pub fn new(
        owner_name: impl Into<String>,
        agent_id: i64,
        agent_name: impl Into<String>,
    ) -> Self {
        Self {
            session_id: None,
            owner_name: owner_name.into(),
            agent: TaskAgentReference {
                id: agent_id,
                name: agent_name.into(),
                version: RUNNER_VERSION.to_string(),
                os_description: std::env::consts::OS.to_string(),
            },
            use_fips_encryption: false,
            encryption_key: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskAgentReference {
    #[serde(rename = "id")]
    pub id: i64,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "osDescription")]
    pub os_description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentSessionKey {
    #[serde(rename = "encrypted")]
    pub encrypted: bool,
    #[serde(rename = "value")]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgent {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, rename = "name", deserialize_with = "null_string_default")]
    pub name: String,
    #[serde(default, rename = "version", deserialize_with = "null_string_default")]
    pub version: String,
    #[serde(
        default,
        rename = "osDescription",
        deserialize_with = "null_string_default"
    )]
    pub os_description: String,
    #[serde(rename = "maxParallelism")]
    pub max_parallelism: i32,
    #[serde(rename = "ephemeral")]
    pub ephemeral: bool,
    #[serde(rename = "disableUpdate")]
    pub disable_update: bool,
    #[serde(rename = "labels")]
    pub labels: Vec<AgentLabel>,
    #[serde(rename = "authorization", skip_serializing_if = "Option::is_none")]
    pub authorization: Option<TaskAgentAuthorization>,
    #[serde(rename = "properties", skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
}

impl TaskAgent {
    pub fn new(
        name: impl Into<String>,
        user_labels: Vec<String>,
        public_key: Option<TaskAgentPublicKey>,
        ephemeral: bool,
    ) -> Self {
        let mut labels = vec![
            AgentLabel::system("self-hosted"),
            AgentLabel::system(std::env::consts::OS),
            AgentLabel::system(std::env::consts::ARCH),
        ];
        labels.extend(user_labels.into_iter().map(AgentLabel::user));

        Self {
            id: None,
            name: name.into(),
            version: RUNNER_VERSION.to_string(),
            os_description: std::env::consts::OS.to_string(),
            max_parallelism: 1,
            ephemeral,
            disable_update: true,
            labels,
            authorization: public_key.map(|public_key| TaskAgentAuthorization {
                authorization_url: None,
                client_id: None,
                public_key: Some(public_key),
            }),
            properties: None,
        }
    }

    pub fn with_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLabel {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: LabelType,
}

impl AgentLabel {
    pub fn system(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            r#type: LabelType::System,
        }
    }

    pub fn user(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            r#type: LabelType::User,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelType {
    #[serde(alias = "system")]
    System,
    #[serde(alias = "user")]
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentAuthorization {
    #[serde(rename = "authorizationUrl", skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(rename = "clientId", skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(rename = "publicKey", skip_serializing_if = "Option::is_none")]
    pub public_key: Option<TaskAgentPublicKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentPublicKey {
    #[serde(rename = "exponent")]
    pub exponent: String,
    #[serde(rename = "modulus")]
    pub modulus: String,
}

impl TaskAgentPublicKey {
    fn from_public_key(public_key: &rsa::RsaPublicKey) -> Self {
        use base64::{engine::general_purpose::STANDARD, Engine};

        Self {
            exponent: STANDARD.encode(public_key.e().to_bytes_be()),
            modulus: STANDARD.encode(public_key.n().to_bytes_be()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub session_id: String,
    pub encryption_key: Option<EncryptionKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionKey {
    pub encrypted: bool,
    pub value_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskAgentMessage {
    #[serde(default, rename = "messageId")]
    pub message_id: i64,
    #[serde(rename = "messageType")]
    pub message_type: String,
    #[serde(rename = "body")]
    pub body: String,
    #[serde(rename = "iv", skip_serializing_if = "Option::is_none")]
    pub iv_base64: Option<String>,
}

pub const RUNNER_JOB_REQUEST: &str = "RunnerJobRequest";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerJobRequestRef {
    #[serde(default, rename = "id")]
    pub id: Option<String>,
    #[serde(rename = "runner_request_id", alias = "runnerRequestId")]
    pub runner_request_id: String,
    #[serde(default, rename = "should_acknowledge", alias = "shouldAcknowledge")]
    pub should_acknowledge: bool,
    #[serde(default, rename = "run_service_url", alias = "runServiceUrl")]
    pub run_service_url: Option<String>,
    #[serde(default, rename = "billing_owner_id", alias = "billingOwnerId")]
    pub billing_owner_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AcquireJobRequest<'a> {
    #[serde(rename = "jobMessageId")]
    job_message_id: &'a str,
    #[serde(rename = "runnerOS")]
    runner_os: &'a str,
    #[serde(rename = "billingOwnerId", skip_serializing_if = "Option::is_none")]
    billing_owner_id: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct RenewJobRequest<'a> {
    #[serde(rename = "planId")]
    plan_id: &'a str,
    #[serde(rename = "jobId")]
    job_id: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenewJobResponse {
    #[serde(rename = "lockedUntil", alias = "LockedUntil")]
    pub locked_until: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunServiceCompleteJob {
    #[serde(rename = "planId")]
    pub plan_id: String,
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "conclusion")]
    pub conclusion: TaskResult,
    #[serde(rename = "outputs", skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, RunServiceVariableValue>,
    #[serde(rename = "stepResults", skip_serializing_if = "Vec::is_empty")]
    pub step_results: Vec<RunServiceStepResult>,
    #[serde(rename = "annotations", skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<RunServiceAnnotation>,
    #[serde(rename = "telemetry", skip_serializing_if = "Vec::is_empty")]
    pub telemetry: Vec<RunServiceTelemetry>,
    #[serde(rename = "environmentUrl", skip_serializing_if = "Option::is_none")]
    pub environment_url: Option<String>,
    #[serde(rename = "billingOwnerId", skip_serializing_if = "Option::is_none")]
    pub billing_owner_id: Option<String>,
    #[serde(
        rename = "infrastructureFailureCategory",
        skip_serializing_if = "Option::is_none"
    )]
    pub infrastructure_failure_category: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunServiceTelemetry {
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunServiceVariableValue {
    #[serde(rename = "value")]
    pub value: String,
    #[serde(rename = "isSecret")]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunServiceStepResult {
    #[serde(rename = "external_id", skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Sequential 1-indexed step number. GitHub uses this for `number` in the
    /// REST API response and for the `/logs/{n}` URL. Maps to `TimelineRecord.Order`.
    #[serde(rename = "number", skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "status")]
    pub status: TimelineRecordState,
    #[serde(rename = "conclusion")]
    pub conclusion: TaskResult,
    #[serde(rename = "started_at", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(rename = "completed_at", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(rename = "completed_log_lines")]
    pub completed_log_lines: i64,
    #[serde(rename = "annotations", skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<RunServiceAnnotation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunServiceAnnotation {
    #[serde(rename = "level")]
    pub level: RunServiceAnnotationLevel,
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "path", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    #[serde(rename = "startColumn", skip_serializing_if = "Option::is_none")]
    pub start_column: Option<i64>,
    #[serde(rename = "endColumn", skip_serializing_if = "Option::is_none")]
    pub end_column: Option<i64>,
    #[serde(rename = "stepNumber", skip_serializing_if = "Option::is_none")]
    pub step_number: Option<i64>,
    #[serde(rename = "isInfrastructureIssue")]
    pub is_infrastructure_issue: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum RunServiceAnnotationLevel {
    #[serde(rename = "notice")]
    Notice,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "failure")]
    Failure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentJobRequest {
    #[serde(rename = "requestId", alias = "RequestId")]
    pub request_id: i64,
    #[serde(
        default,
        rename = "lockedUntil",
        alias = "LockedUntil",
        skip_serializing_if = "Option::is_none"
    )]
    pub locked_until: Option<String>,
    #[serde(
        default,
        rename = "finishTime",
        alias = "FinishTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub finish_time: Option<String>,
    #[serde(
        default,
        rename = "result",
        alias = "Result",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<TaskResult>,
    #[serde(
        default,
        rename = "jobId",
        alias = "JobId",
        skip_serializing_if = "Option::is_none"
    )]
    pub job_id: Option<String>,
    #[serde(
        default,
        rename = "jobName",
        alias = "JobName",
        skip_serializing_if = "Option::is_none"
    )]
    pub job_name: Option<String>,
}

impl TaskAgentJobRequest {
    pub fn renew(request_id: i64) -> Self {
        Self {
            request_id,
            locked_until: None,
            finish_time: None,
            result: None,
            job_id: None,
            job_name: None,
        }
    }

    pub fn finish(request_id: i64, finish_time_utc: impl Into<String>, result: TaskResult) -> Self {
        Self {
            request_id,
            locked_until: None,
            finish_time: Some(finish_time_utc.into()),
            result: Some(result),
            job_id: None,
            job_name: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompletedEvent {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "requestId")]
    pub request_id: i64,
    #[serde(rename = "result")]
    pub result: TaskResult,
    #[serde(
        default,
        rename = "outputs",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub outputs: BTreeMap<String, JobOutputValue>,
}

impl JobCompletedEvent {
    pub fn new(
        request_id: i64,
        job_id: impl Into<String>,
        result: TaskResult,
        outputs: BTreeMap<String, String>,
    ) -> Self {
        Self {
            name: "JobCompleted".to_string(),
            job_id: job_id.into(),
            request_id,
            result,
            outputs: outputs
                .into_iter()
                .map(|(name, value)| {
                    (
                        name,
                        JobOutputValue {
                            value: Some(value),
                            is_secret: false,
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutputValue {
    #[serde(default, rename = "value", skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, rename = "isSecret")]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineRecord {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(default, rename = "parentId", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(rename = "type")]
    pub record_type: TimelineRecordType,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(default, rename = "startTime", skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(
        default,
        rename = "finishTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub finish_time: Option<String>,
    #[serde(
        default,
        rename = "currentOperation",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_operation: Option<String>,
    #[serde(
        default,
        rename = "percentComplete",
        skip_serializing_if = "Option::is_none"
    )]
    pub percent_complete: Option<i32>,
    #[serde(default, rename = "state", skip_serializing_if = "Option::is_none")]
    pub state: Option<TimelineRecordState>,
    #[serde(default, rename = "result", skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(
        default,
        rename = "workerName",
        skip_serializing_if = "Option::is_none"
    )]
    pub worker_name: Option<String>,
    #[serde(default, rename = "order", skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    #[serde(default, rename = "refName", skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    #[serde(default, rename = "errorCount")]
    pub error_count: i32,
    #[serde(default, rename = "warningCount")]
    pub warning_count: i32,
    #[serde(default, rename = "noticeCount")]
    pub notice_count: i32,
}

impl TimelineRecord {
    pub fn job_pending(
        job_id: impl Into<String>,
        name: impl Into<String>,
        ref_name: Option<String>,
        worker_name: impl Into<String>,
    ) -> Self {
        Self {
            id: job_id.into(),
            parent_id: None,
            record_type: TimelineRecordType::Job,
            name: name.into(),
            start_time: None,
            finish_time: None,
            current_operation: None,
            percent_complete: Some(0),
            state: Some(TimelineRecordState::Pending),
            result: None,
            worker_name: Some(worker_name.into()),
            order: None,
            ref_name,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
        }
    }

    pub fn task_completed(
        step_id: impl Into<String>,
        parent_id: impl Into<String>,
        name: impl Into<String>,
        order: i32,
        finish_time: impl Into<String>,
        result: TaskResult,
    ) -> Self {
        Self {
            id: step_id.into(),
            parent_id: Some(parent_id.into()),
            record_type: TimelineRecordType::Task,
            name: name.into(),
            start_time: None,
            finish_time: Some(finish_time.into()),
            current_operation: None,
            percent_complete: Some(100),
            state: Some(TimelineRecordState::Completed),
            result: Some(result),
            worker_name: None,
            order: Some(order),
            ref_name: None,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
        }
    }

    pub fn task_pending(
        step_id: impl Into<String>,
        parent_id: impl Into<String>,
        name: impl Into<String>,
        order: i32,
    ) -> Self {
        Self {
            id: step_id.into(),
            parent_id: Some(parent_id.into()),
            record_type: TimelineRecordType::Task,
            name: name.into(),
            start_time: None,
            finish_time: None,
            current_operation: None,
            percent_complete: Some(0),
            state: Some(TimelineRecordState::Pending),
            result: None,
            worker_name: None,
            order: Some(order),
            ref_name: None,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
        }
    }

    pub fn with_issue_counts(
        mut self,
        error_count: i32,
        warning_count: i32,
        notice_count: i32,
    ) -> Self {
        self.error_count = error_count;
        self.warning_count = warning_count;
        self.notice_count = notice_count;
        self
    }

    pub fn in_progress(mut self, start_time: impl Into<String>) -> Self {
        self.start_time = Some(start_time.into());
        self.state = Some(TimelineRecordState::InProgress);
        self
    }

    pub fn completed(mut self, finish_time: impl Into<String>, result: TaskResult) -> Self {
        self.finish_time = Some(finish_time.into());
        self.percent_complete = Some(100);
        self.state = Some(TimelineRecordState::Completed);
        self.result = Some(result);
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TimelineRecordType {
    #[serde(rename = "Job", alias = "job")]
    Job,
    #[serde(rename = "Task", alias = "task")]
    Task,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TimelineRecordState {
    #[serde(rename = "pending", alias = "Pending")]
    Pending,
    #[serde(rename = "inProgress", alias = "InProgress")]
    InProgress,
    #[serde(rename = "completed", alias = "Completed")]
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineRecordFeedLines {
    #[serde(rename = "stepId")]
    pub step_id: String,
    #[serde(rename = "value")]
    pub value: Vec<String>,
    #[serde(default, rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
}

impl TimelineRecordFeedLines {
    pub fn new(step_id: impl Into<String>, lines: Vec<String>, start_line: Option<i64>) -> Self {
        Self {
            step_id: step_id.into(),
            value: lines,
            start_line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskResult {
    #[serde(rename = "succeeded", alias = "Succeeded")]
    Succeeded,
    #[serde(rename = "failed", alias = "Failed")]
    Failed,
    #[serde(rename = "canceled", alias = "Canceled")]
    Canceled,
    #[serde(rename = "skipped", alias = "Skipped")]
    Skipped,
    #[serde(rename = "abandoned", alias = "Abandoned")]
    Abandoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStatus {
    Online,
    Busy,
    Offline,
}

impl RunnerStatus {
    pub fn as_query_value(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Busy => "Busy",
            Self::Offline => "Offline",
        }
    }
}

pub trait GitHubRunnerProtocol {
    async fn create_session(&self) -> anyhow::Result<AgentSession>;
    async fn next_message(
        &self,
        session: &AgentSession,
        last_message_id: Option<i64>,
        status: RunnerStatus,
    ) -> anyhow::Result<Option<TaskAgentMessage>>;
    async fn delete_message(&self, session: &AgentSession, message_id: i64) -> anyhow::Result<()>;
    async fn renew_job(&self, request_id: i64) -> anyhow::Result<()>;
    async fn finish_job(&self, request_id: i64, result: TaskResult) -> anyhow::Result<()>;
}

// ── Results Service: WebSocket live console feed ──────────────────────────────

/// Log lines batch sent over the WebSocket feed stream.
/// Matches the GitHub Actions `TimelineRecordFeedLinesWrapper` wire format.
#[derive(Debug, Clone, Serialize)]
pub struct FeedLines {
    // Field order and names match exactly what GitHub's actions/runner sends.
    // See TimelineRecordFeedLinesWrapper in the runner source.
    pub count: usize,
    pub value: Vec<String>,
    #[serde(rename = "stepId")]
    pub step_id: String,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
    // planId/jobId needed for routing in the Results Service.
    #[serde(rename = "planId", skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(rename = "jobId", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

/// WebSocket client for streaming live console output to the GitHub Results Service.
/// Connects to `FeedStreamUrl` from the `SystemVssConnection` endpoint data.
///
/// GitHub Actions V2 runner maintains ONE persistent WebSocket connection per job.
/// All step log lines flow through this single connection tagged by stepId.
pub struct FeedStreamClient {
    url: String,
    token: String,
    plan_id: Option<String>,
    job_id: Option<String>,
}

impl FeedStreamClient {
    pub fn new(feed_stream_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url: feed_stream_url.into(),
            token: token.into(),
            plan_id: None,
            job_id: None,
        }
    }

    pub fn with_context(mut self, plan_id: &str, job_id: &str) -> Self {
        self.plan_id = Some(plan_id.to_string());
        self.job_id = Some(job_id.to_string());
        self
    }

    /// Try to create a FeedStreamClient from the SystemVssConnection endpoint data.
    pub fn from_endpoint_data(data: &BTreeMap<String, String>, token: &str) -> Option<Self> {
        let url = data.get("FeedStreamUrl")?.clone();
        if url.is_empty() || token.is_empty() {
            return None;
        }
        Some(Self::new(url, token))
    }

    /// Open a persistent WebSocket connection for the job's entire log stream.
    /// The official GitHub runner keeps this connection open for the whole job.
    pub async fn connect(
        &self,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        use tokio_tungstenite::connect_async;
        // Append plan_id and job_id as query parameters so the Results Service
        // can route the connection to the correct run's blob storage.
        let conn_url = if let (Some(plan_id), Some(job_id)) = (&self.plan_id, &self.job_id) {
            let sep = if self.url.contains('?') { '&' } else { '?' };
            format!("{}{sep}planId={plan_id}&jobId={job_id}", self.url)
        } else {
            self.url.clone()
        };
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .method("GET")
            .uri(&conn_url)
            .header("Host", ws_host(&self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", RUNNER_USER_AGENT)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                STANDARD.encode(uuid::Uuid::new_v4().as_bytes()),
            )
            .body(())
            .context("build WebSocket request")?;
        let (ws, _) = connect_async(request)
            .await
            .context("connect to feed stream WebSocket")?;
        Ok(ws)
    }

    /// Send log lines for a step over an existing persistent WebSocket connection.
    /// Matches the official GitHub runner: 1KB text chunks, count field required.
    pub async fn send_log_lines(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        step_id: &str,
        lines: Vec<String>,
        start_line: Option<i64>,
        plan_id: Option<&str>,
        _job_id: Option<&str>,
    ) -> Result<()> {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        let count = lines.len();
        let feed = FeedLines {
            count,
            value: lines,
            step_id: step_id.to_string(),
            start_line,
            plan_id: plan_id.map(|s| s.to_string()),
            job_id: _job_id.map(|s| s.to_string()),
        };
        let json = serde_json::to_string(&feed)?;
        ws.send(Message::Text(json.into()))
            .await
            .context("send WebSocket log lines")?;
        Ok(())
    }

    /// Send a WebSocket ping to keep the feed connection warm during idle gaps
    /// (e.g. a long compile step that emits no log lines). Without periodic
    /// traffic GitHub closes the idle connection and the next log send hits a
    /// Broken pipe, making the live console stutter.
    pub async fn send_ping(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Result<()> {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        ws.send(Message::Ping(Vec::new().into()))
            .await
            .context("send WebSocket keepalive ping")?;
        Ok(())
    }

    /// Legacy per-call method. Prefer connect() + send_log_lines() for jobs.
    pub async fn append_log_lines(
        &self,
        step_id: &str,
        lines: Vec<String>,
        start_line: Option<i64>,
    ) -> Result<()> {
        let mut ws = self.connect().await?;
        Self::send_log_lines(&mut ws, step_id, lines, start_line, None, None).await?;
        ws.close(None).await.ok();
        Ok(())
    }
}

// ── Results Service: Twirp step status updates ────────────────────────────────

/// Step status values (matches GitHub Actions Results Service proto enum).
#[derive(Debug, Clone, Copy, Serialize)]
pub enum StepStatus {
    #[serde(rename = "3")]
    InProgress = 3,
    #[serde(rename = "6")]
    Completed = 6,
}

/// Step conclusion values.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum StepConclusion {
    #[serde(rename = "0")]
    Unknown = 0,
    #[serde(rename = "2")]
    Success = 2,
    #[serde(rename = "3")]
    Failure = 3,
    #[serde(rename = "4")]
    Cancelled = 4,
    #[serde(rename = "7")]
    Skipped = 7,
}

/// A step record sent to the Twirp WorkflowStepsUpdate endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct TwirpStep {
    pub external_id: String,
    pub number: usize,
    pub name: String,
    pub status: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub conclusion: u8,
}

/// Request body for `WorkflowStepsUpdate` Twirp call.
#[derive(Debug, Serialize)]
struct WorkflowStepsUpdateRequest<'a> {
    steps: &'a [TwirpStep],
    change_order: i64,
    workflow_job_run_backend_id: &'a str,
    workflow_run_backend_id: &'a str,
}

/// Client for the GitHub Actions Results Service Twirp API.
pub struct TwirpResultsClient {
    results_service_url: String,
    token: String,
    http: Client,
}

impl TwirpResultsClient {
    pub fn new(results_service_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        Ok(Self {
            results_service_url: results_service_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: Client::builder()
                .user_agent(RUNNER_USER_AGENT)
                .build()
                .context("build Twirp HTTP client")?,
        })
    }

    /// Create from SystemVssConnection endpoint data if ResultsServiceUrl is present.
    pub fn from_endpoint_data(
        data: &BTreeMap<String, String>,
        token: &str,
    ) -> Option<Result<Self>> {
        let url = data.get("ResultsServiceUrl")?.clone();
        if url.is_empty() || token.is_empty() {
            return None;
        }
        Some(Self::new(url, token))
    }

    /// Send step status updates via `WorkflowStepsUpdate`.
    pub async fn update_steps(
        &self,
        steps: &[TwirpStep],
        workflow_run_backend_id: &str,
        workflow_job_run_backend_id: &str,
        change_order: i64,
    ) -> Result<()> {
        let url = format!(
            "{}/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate",
            self.results_service_url
        );
        let body = WorkflowStepsUpdateRequest {
            steps,
            change_order,
            workflow_job_run_backend_id,
            workflow_run_backend_id,
        };
        // Route through the selected transport: GitHub has throttled
        // reqwest/hyper by TLS fingerprint (native-tls/OpenSSL) under heavy
        // concurrent load, which silently dropped step records (the job's step
        // list went incomplete in the UI). Curl remains the default fallback.
        // Retry a couple times so a transient blip never loses a step record.
        let body_json = serde_json::to_string(&body).context("serialize WorkflowStepsUpdate")?;
        let mut last_err = String::new();
        for attempt in 0..3 {
            match github_json_request("POST", &url, &self.token, Some(body_json.clone()), 30).await
            {
                Ok((status, _)) if (200..300).contains(&status) => return Ok(()),
                Ok((status, resp)) => last_err = format!("status={status}, body={resp}"),
                Err(e) => last_err = e.to_string(),
            }
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
        bail!("WorkflowStepsUpdate failed after 3 attempts: {last_err}");
    }

    /// Upload step log content to Results Service blob storage.
    ///
    /// Flow (matches official runner `UploadResultsStepLogAsync`):
    ///   1. Get a signed blob URL from `GetStepLogsSignedBlobURL`
    ///   2. PUT the log content to that URL as `text/plain`
    ///   3. Finalise with `CreateStepLogsMetadata`
    pub async fn upload_step_log(
        &self,
        plan_id: &str,
        job_id: &str,
        step_id: &str,
        lines: &[String],
    ) -> Result<()> {
        const RECEIVER: &str = "twirp/results.services.receiver.Receiver";

        #[derive(serde::Serialize)]
        struct GetUrlReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            step_backend_id: &'a str,
        }
        #[derive(serde::Deserialize)]
        struct GetUrlResp {
            logs_url: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct MetaReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            step_backend_id: &'a str,
            uploaded_at: String,
            line_count: i64,
        }
        let get_url = format!(
            "{}/{RECEIVER}/GetStepLogsSignedBlobURL",
            self.results_service_url
        );
        let meta_url = format!(
            "{}/{RECEIVER}/CreateStepLogsMetadata",
            self.results_service_url
        );
        let get_body = serde_json::to_string(&GetUrlReq {
            workflow_run_backend_id: plan_id,
            workflow_job_run_backend_id: job_id,
            step_backend_id: step_id,
        })
        .context("serialize GetStepLogsSignedBlobURL")?;
        let content: Vec<u8> = lines
            .iter()
            .flat_map(|l| format!("{l}\n").into_bytes())
            .collect();
        let line_count = lines.len() as i64;

        // The two Twirp calls hit GitHub infra and are throttled by TLS
        // fingerprint under heavy concurrent load — if they drop, the step
        // renders with an EMPTY log body (less detail than GitHub). Route them
        // through the selected transport and retry the whole flow so log content
        // always lands. The PUT goes to Azure blob storage (not GitHub,
        // not throttled), so it stays on reqwest.
        let mut last_err = String::new();
        for attempt in 0..3 {
            match self
                .upload_step_log_once(
                    &get_url, &meta_url, &get_body, &content, line_count, plan_id, job_id, step_id,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => last_err = format!("{e:#}"),
            }
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
        bail!("upload_step_log failed after 3 attempts: {last_err}");
    }

    /// Upload the combined job log to Results Service blob storage.
    ///
    /// Flow matches official runner `UploadResultsJobLogAsync`:
    ///   1. Get a signed blob URL from `GetJobLogsSignedBlobURL`
    ///   2. PUT the log content to that URL as `text/plain`
    ///   3. Finalise with `CreateJobLogsMetadata`
    pub async fn upload_job_log(
        &self,
        plan_id: &str,
        job_id: &str,
        content: &[u8],
        line_count: i64,
    ) -> Result<()> {
        const RECEIVER: &str = "twirp/results.services.receiver.Receiver";

        #[derive(serde::Serialize)]
        struct GetUrlReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
        }

        let get_url = format!(
            "{}/{RECEIVER}/GetJobLogsSignedBlobURL",
            self.results_service_url
        );
        let meta_url = format!(
            "{}/{RECEIVER}/CreateJobLogsMetadata",
            self.results_service_url
        );
        let get_body = serde_json::to_string(&GetUrlReq {
            workflow_run_backend_id: plan_id,
            workflow_job_run_backend_id: job_id,
        })
        .context("serialize GetJobLogsSignedBlobURL")?;

        let mut last_err = String::new();
        for attempt in 0..3 {
            match self
                .upload_job_log_once(
                    &get_url, &meta_url, &get_body, content, line_count, plan_id, job_id,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => last_err = format!("{e:#}"),
            }
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
        bail!("upload_job_log failed after 3 attempts: {last_err}");
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_job_log_once(
        &self,
        get_url: &str,
        meta_url: &str,
        get_body: &str,
        content: &[u8],
        line_count: i64,
        plan_id: &str,
        job_id: &str,
    ) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct GetUrlResp {
            logs_url: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct MetaReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            uploaded_at: String,
            line_count: i64,
        }
        #[derive(serde::Deserialize)]
        struct MetaResp {
            ok: bool,
        }

        let (status, body) =
            github_json_request("POST", get_url, &self.token, Some(get_body.to_string()), 30)
                .await
                .context("GetJobLogsSignedBlobURL request")?;
        if !(200..300).contains(&status) {
            bail!("GetJobLogsSignedBlobURL failed: status={status}, body={body}");
        }
        let resp: GetUrlResp =
            serde_json::from_str(&body).context("GetJobLogsSignedBlobURL parse")?;
        let logs_url = resp
            .logs_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GetJobLogsSignedBlobURL returned empty URL"))?;

        let put_resp = self
            .http
            .put(&logs_url)
            .header("Content-Type", "text/plain")
            .header("Content-Length", content.len().to_string())
            .header("x-ms-blob-type", "BlockBlob")
            .body(content.to_vec())
            .send()
            .await
            .context("job log PUT")?;
        let put_status = put_resp.status();
        if !put_status.is_success() {
            let body = put_resp.text().await.unwrap_or_default();
            bail!("job log PUT failed: status={put_status}, body={body}");
        }

        let ts = {
            use time::{format_description::well_known::Rfc3339, OffsetDateTime};
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
        };
        let meta_body = serde_json::to_string(&MetaReq {
            workflow_run_backend_id: plan_id,
            workflow_job_run_backend_id: job_id,
            uploaded_at: ts,
            line_count,
        })
        .context("serialize CreateJobLogsMetadata")?;
        let (meta_status, meta_body_resp) =
            github_json_request("POST", meta_url, &self.token, Some(meta_body), 30)
                .await
                .context("CreateJobLogsMetadata request")?;
        if !(200..300).contains(&meta_status) {
            bail!("CreateJobLogsMetadata failed: status={meta_status}, body={meta_body_resp}");
        }
        let meta_resp: MetaResp =
            serde_json::from_str(&meta_body_resp).context("CreateJobLogsMetadata parse")?;
        if !meta_resp.ok {
            bail!("CreateJobLogsMetadata returned ok=false: body={meta_body_resp}");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_step_log_once(
        &self,
        get_url: &str,
        meta_url: &str,
        get_body: &str,
        content: &[u8],
        line_count: i64,
        plan_id: &str,
        job_id: &str,
        step_id: &str,
    ) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct GetUrlResp {
            logs_url: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct MetaReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            step_backend_id: &'a str,
            uploaded_at: String,
            line_count: i64,
        }
        #[derive(serde::Deserialize)]
        struct MetaResp {
            ok: bool,
        }

        // 1. Get signed upload URL through the selected GitHub transport.
        let (status, body) =
            github_json_request("POST", get_url, &self.token, Some(get_body.to_string()), 30)
                .await
                .context("GetStepLogsSignedBlobURL request")?;
        if !(200..300).contains(&status) {
            bail!("GetStepLogsSignedBlobURL failed: status={status}, body={body}");
        }
        let resp: GetUrlResp =
            serde_json::from_str(&body).context("GetStepLogsSignedBlobURL parse")?;
        let logs_url = resp
            .logs_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GetStepLogsSignedBlobURL returned empty URL"))?;

        // 2. PUT log content to Azure blob (single block; reqwest — not GitHub infra).
        let put_resp = self
            .http
            .put(&logs_url)
            .header("Content-Type", "text/plain")
            .header("Content-Length", content.len().to_string())
            .header("x-ms-blob-type", "BlockBlob")
            .body(content.to_vec())
            .send()
            .await
            .context("step log PUT")?;
        let put_status = put_resp.status();
        if !put_status.is_success() {
            let body = put_resp.text().await.unwrap_or_default();
            bail!("step log PUT failed: status={put_status}, body={body}");
        }

        // 3. Finalize with metadata through the selected GitHub transport.
        let ts = {
            use time::{format_description::well_known::Rfc3339, OffsetDateTime};
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
        };
        let meta_body = serde_json::to_string(&MetaReq {
            workflow_run_backend_id: plan_id,
            workflow_job_run_backend_id: job_id,
            step_backend_id: step_id,
            uploaded_at: ts,
            line_count,
        })
        .context("serialize CreateStepLogsMetadata")?;
        let (meta_status, meta_body_resp) =
            github_json_request("POST", meta_url, &self.token, Some(meta_body), 30)
                .await
                .context("CreateStepLogsMetadata request")?;
        if !(200..300).contains(&meta_status) {
            bail!("CreateStepLogsMetadata failed: status={meta_status}, body={meta_body_resp}");
        }
        let meta_resp: MetaResp =
            serde_json::from_str(&meta_body_resp).context("CreateStepLogsMetadata parse")?;
        if !meta_resp.ok {
            bail!("CreateStepLogsMetadata returned ok=false: body={meta_body_resp}");
        }
        Ok(())
    }

    /// Upload GITHUB_STEP_SUMMARY content to the Results Service so it renders
    /// in the GitHub UI "Summary" tab. Follows the same signed-URL flow as step
    /// log upload: GetStepSummarySignedBlobURL → PUT → CreateStepSummaryMetadata.
    pub async fn upload_step_summary(
        &self,
        plan_id: &str,
        job_id: &str,
        step_id: &str,
        content: &str,
    ) -> Result<()> {
        const RECEIVER: &str = "twirp/results.services.receiver.Receiver";

        // 1. Get signed upload URL.
        let url = format!(
            "{}/{RECEIVER}/GetStepSummarySignedBlobURL",
            self.results_service_url
        );
        #[derive(serde::Serialize)]
        struct GetUrlReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            step_backend_id: &'a str,
        }
        #[derive(serde::Deserialize)]
        struct GetUrlResp {
            blob_url: Option<String>,
        }
        let resp: GetUrlResp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&GetUrlReq {
                workflow_run_backend_id: plan_id,
                workflow_job_run_backend_id: job_id,
                step_backend_id: step_id,
            })
            .send()
            .await
            .context("GetStepSummarySignedBlobURL request")?
            .json()
            .await
            .context("GetStepSummarySignedBlobURL parse")?;

        let blob_url = resp
            .blob_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GetStepSummarySignedBlobURL returned empty URL"))?;

        // 2. Upload summary content.
        let content_bytes = content.as_bytes().to_vec();
        let content_len = content_bytes.len();
        let put_resp = self
            .http
            .put(&blob_url)
            .header("Content-Type", "text/plain")
            .header("Content-Length", content_len.to_string())
            .header("x-ms-blob-type", "BlockBlob")
            .body(content_bytes)
            .send()
            .await
            .context("step summary PUT")?;
        let put_status = put_resp.status();
        if !put_status.is_success() {
            let body = put_resp.text().await.unwrap_or_default();
            bail!("step summary PUT failed: status={put_status}, body={body}");
        }

        // 3. Finalize with metadata.
        let url = format!(
            "{}/{RECEIVER}/CreateStepSummaryMetadata",
            self.results_service_url
        );
        #[derive(serde::Serialize)]
        struct MetaReq<'a> {
            workflow_run_backend_id: &'a str,
            workflow_job_run_backend_id: &'a str,
            step_backend_id: &'a str,
            uploaded_at: String,
        }
        use time::{format_description::well_known::Rfc3339, OffsetDateTime};
        let ts = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        let meta_resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&MetaReq {
                workflow_run_backend_id: plan_id,
                workflow_job_run_backend_id: job_id,
                step_backend_id: step_id,
                uploaded_at: ts,
            })
            .send()
            .await
            .context("CreateStepSummaryMetadata request")?;
        let meta_status = meta_resp.status();
        if !meta_status.is_success() {
            let body = meta_resp.text().await.unwrap_or_default();
            bail!("CreateStepSummaryMetadata failed: status={meta_status}, body={body}");
        }
        Ok(())
    }
}

fn artifact_zip_bytes(files: &[(String, Vec<u8>)], store_uncompressed: bool) -> Result<Vec<u8>> {
    use std::io::Write;

    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let method = if store_uncompressed {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    let options = zip::write::FileOptions::<()>::default().compression_method(method);
    for (archive_path, content) in files {
        zip.start_file(archive_path, options)
            .context("zip start_file")?;
        zip.write_all(content).context("zip write")?;
    }
    Ok(zip.finish().context("zip finish")?.into_inner())
}

fn artifact_create_request(
    plan_id: &str,
    job_id: &str,
    name: &str,
    retention_days: Option<u8>,
    now: time::OffsetDateTime,
) -> Result<serde_json::Value> {
    let mut request = serde_json::json!({
        "workflow_run_backend_id": plan_id,
        "workflow_job_run_backend_id": job_id,
        "name": name,
        "version": 4
    });
    if let Some(days) = retention_days {
        let expires_at = (now + time::Duration::days(i64::from(days)))
            .format(&time::format_description::well_known::Rfc3339)
            .context("format artifact expiration")?;
        request["expires_at"] = serde_json::Value::String(expires_at);
    }
    Ok(request)
}

/// Upload artifact files to GitHub's Results Service (artifact v4 format).
///
/// Uses synchronous `reqwest::blocking` — safe to call from `tokio::task::spawn_blocking`
/// threads (the Velnor job executor context).
///
/// Flow: CreateArtifact → PUT zip to signed URL → FinalizeArtifact
#[derive(Clone, Copy, Debug, Default)]
pub struct ArtifactUploadOptions {
    pub store_uncompressed: bool,
    pub retention_days: Option<u8>,
}

pub fn upload_artifact_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    files: &[(String, Vec<u8>)], // (archive path, content)
    options: ArtifactUploadOptions,
) -> Result<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    let base = results_service_url.trim_end_matches('/');
    let tmp_dir = std::env::temp_dir();

    // Write a mode-0600 file and return its path. Caller must delete.
    let write_secret_file = |suffix: &str, content: &[u8]| -> std::io::Result<std::path::PathBuf> {
        let p = tmp_dir.join(format!("velnor-artifact-{}.{suffix}", uuid::Uuid::new_v4()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&p)?;
        f.write_all(content)?;
        Ok(p)
    };

    // Helper: curl POST with JSON via 0600 config + body files — secrets stay off argv.
    let curl_post = |url: &str, body: &str| -> Result<String> {
        let cfg = format!(
            "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
             header = \"Authorization: Bearer {token}\"\n\
             header = \"Accept: application/json\"\n\
             header = \"Content-Type: application/json\"\n\
             max-time = 30\n\
             request = POST\n\
             silent\n\
             write-out = \"\\n%{{http_code}}\"\n"
        );
        let cfg_path = write_secret_file("cfg", cfg.as_bytes()).context("write curl cfg")?;
        let body_path = write_secret_file("body", body.as_bytes()).context("write curl body")?;
        let out = std::process::Command::new("curl")
            .arg("--config")
            .arg(&cfg_path)
            .arg("--data")
            .arg(format!("@{}", body_path.display()))
            .arg(url)
            .output();
        let _ = std::fs::remove_file(&cfg_path);
        let _ = std::fs::remove_file(&body_path);
        let out = out.context("run curl")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (resp_body, status_str) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
        let status: u16 = status_str.trim().parse().unwrap_or(0);
        if !(200..300).contains(&status) {
            bail!("curl POST {url}: status={status}, body={resp_body}");
        }
        Ok(resp_body.to_string())
    };

    // 1. CreateArtifact → signed upload URL.
    let create_url = format!("{base}/{SERVICE}/CreateArtifact");
    let create_request = artifact_create_request(
        plan_id,
        job_id,
        name,
        options.retention_days,
        time::OffsetDateTime::now_utc(),
    )?;
    let create_body = serde_json::to_string(&create_request).context("serialize CreateArtifact")?;
    let create_text = curl_post(&create_url, &create_body).context("CreateArtifact request")?;
    let create_resp: serde_json::Value =
        serde_json::from_str(&create_text).context("CreateArtifact parse")?;
    if create_resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        bail!("CreateArtifact: backend returned ok=false or absent");
    }
    let upload_url = create_resp
        .get("signed_upload_url")
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty())
        .context("CreateArtifact: empty signed_upload_url")?
        .to_string();

    // 2. Create zip archive and PUT to signed URL.
    // The signed URL is itself a credential — keep it off argv via --config.
    let zip_bytes = artifact_zip_bytes(files, options.store_uncompressed)?;
    let zip_size = zip_bytes.len() as u64;

    let zip_path = write_secret_file("zip", &zip_bytes).context("write zip temp file")?;
    let put_cfg = format!(
        "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
         header = \"Content-Type: application/zip\"\n\
         header = \"Content-Length: {zip_size}\"\n\
         header = \"x-ms-blob-type: BlockBlob\"\n\
         max-time = 60\n\
         request = PUT\n\
         silent\n\
         write-out = \"\\n%{{http_code}}\"\n\
         url = \"{upload_url}\"\n"
    );
    let put_cfg_path = write_secret_file("put.cfg", put_cfg.as_bytes()).context("write PUT cfg")?;
    let put_out = std::process::Command::new("curl")
        .arg("--config")
        .arg(&put_cfg_path)
        .arg("--data-binary")
        .arg(format!("@{}", zip_path.display()))
        .output();
    let _ = std::fs::remove_file(&put_cfg_path);
    let _ = std::fs::remove_file(&zip_path);
    let put_out = put_out.context("run curl PUT")?;
    let stdout = String::from_utf8_lossy(&put_out.stdout);
    let (_, status_str) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_ref()));
    let put_status: u16 = status_str.trim().parse().unwrap_or(0);
    if !(200..300).contains(&put_status) {
        bail!("artifact blob PUT failed: status={put_status}");
    }

    // 3. FinalizeArtifact.
    let finalize_url = format!("{base}/{SERVICE}/FinalizeArtifact");
    let finalize_body = serde_json::to_string(&serde_json::json!({
        "workflow_run_backend_id": plan_id,
        "workflow_job_run_backend_id": job_id,
        "name": name,
        "size": zip_size.to_string()
    }))
    .context("serialize FinalizeArtifact")?;
    let finalize_text =
        curl_post(&finalize_url, &finalize_body).context("FinalizeArtifact request")?;
    let finalize: serde_json::Value =
        serde_json::from_str(&finalize_text).context("FinalizeArtifact parse")?;
    let artifact_id = finalize
        .get("artifact_id")
        .or_else(|| finalize.get("artifactId"))
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.clone()),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .context("FinalizeArtifact: missing artifact_id")?;
    Ok(artifact_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultsArtifactDownload {
    pub name: String,
    pub files: Vec<(std::path::PathBuf, Vec<u8>)>,
}

/// Download artifacts visible to this workflow run through the Results
/// Service v4 protocol used by `actions/download-artifact`.
///
/// `name` (exact) or `pattern` (glob) filter the artifact list BEFORE anything
/// is signed or downloaded: `docker/build-push-action` stores `.dockerbuild`
/// build-record artifacts as gzip blobs rather than zips, so downloading every
/// listed artifact unconditionally fails any download-artifact step in such a
/// run with "invalid Zip archive: EOCD". Non-zip artifacts that still pass the
/// filter (e.g. an unfiltered download-all in the same run) are skipped with a
/// warning instead of failing the step.
///
/// Flow: ListArtifacts -> GetSignedArtifactURL -> GET zip. Signed URLs and the
/// runtime bearer token are supplied through mode-0600 curl config files so
/// neither credential appears in process arguments.
pub fn download_artifacts_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    pattern: &str,
) -> Result<Vec<ResultsArtifactDownload>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    let base = results_service_url.trim_end_matches('/');
    let tmp_dir = std::env::temp_dir();

    let matcher = if !name.is_empty() || pattern.is_empty() {
        None
    } else {
        let mut builder = globset::GlobSetBuilder::new();
        builder.add(globset::Glob::new(pattern)?);
        Some(builder.build().context("build artifact pattern")?)
    };

    let write_secret_file = |suffix: &str, content: &[u8]| -> std::io::Result<std::path::PathBuf> {
        let path = tmp_dir.join(format!(
            "velnor-artifact-download-{}.{suffix}",
            uuid::Uuid::new_v4()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        std::io::Write::write_all(&mut file, content)?;
        Ok(path)
    };

    let curl_post = |method: &str, body: &serde_json::Value| -> Result<serde_json::Value> {
        let url = format!("{base}/{SERVICE}/{method}");
        let config = format!(
            "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
             header = \"Authorization: Bearer {token}\"\n\
             header = \"Accept: application/json\"\n\
             header = \"Content-Type: application/json\"\n\
             max-time = 30\n\
             request = POST\n\
             silent\n\
             write-out = \"\\n%{{http_code}}\"\n"
        );
        let config_path = write_secret_file("cfg", config.as_bytes())?;
        let body_path = write_secret_file("json", serde_json::to_string(body)?.as_bytes())?;
        let output = std::process::Command::new("curl")
            .arg("--config")
            .arg(&config_path)
            .arg("--data")
            .arg(format!("@{}", body_path.display()))
            .arg(&url)
            .output();
        let _ = std::fs::remove_file(&config_path);
        let _ = std::fs::remove_file(&body_path);
        let output = output.with_context(|| format!("run Results Service {method}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (response, status_text) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
        let status: u16 = status_text.trim().parse().unwrap_or(0);
        if !(200..300).contains(&status) {
            bail!("Results Service {method}: status={status}, body={response}");
        }
        serde_json::from_str(response).with_context(|| format!("parse Results Service {method}"))
    };

    let listed = curl_post(
        "ListArtifacts",
        &serde_json::json!({
            "workflow_run_backend_id": plan_id,
            "workflow_job_run_backend_id": job_id
        }),
    )?;
    let artifacts = listed
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut downloads = Vec::new();
    for artifact in artifacts {
        let Some(artifact_name) = artifact
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        // Filter BEFORE signing/downloading: unrequested artifacts (notably
        // non-zip `.dockerbuild` build records) must never be fetched.
        let selected = if !name.is_empty() {
            artifact_name == name
        } else if let Some(matcher) = &matcher {
            matcher.is_match(artifact_name)
        } else {
            true
        };
        if !selected {
            continue;
        }
        let artifact_plan = artifact
            .get("workflow_run_backend_id")
            .or_else(|| artifact.get("workflowRunBackendId"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(plan_id);
        let artifact_job = artifact
            .get("workflow_job_run_backend_id")
            .or_else(|| artifact.get("workflowJobRunBackendId"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(job_id);
        let signed = curl_post(
            "GetSignedArtifactURL",
            &serde_json::json!({
                "workflow_run_backend_id": artifact_plan,
                "workflow_job_run_backend_id": artifact_job,
                "name": artifact_name
            }),
        )?;
        let signed_url = signed
            .get("signed_url")
            .or_else(|| signed.get("signedUrl"))
            .and_then(serde_json::Value::as_str)
            .filter(|url| !url.is_empty())
            .context("GetSignedArtifactURL returned no signed URL")?;
        let zip_path = tmp_dir.join(format!(
            "velnor-artifact-download-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let get_config = format!(
            "header = \"User-Agent: {RUNNER_USER_AGENT}\"\n\
             max-time = 120\n\
             location\n\
             fail\n\
             silent\n\
             show-error\n\
             url = \"{signed_url}\"\n"
        );
        let config_path = write_secret_file("get.cfg", get_config.as_bytes())?;
        let output = std::process::Command::new("curl")
            .arg("--config")
            .arg(&config_path)
            .arg("--output")
            .arg(&zip_path)
            .output();
        let _ = std::fs::remove_file(&config_path);
        let output = output.context("download Results Service artifact zip")?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&zip_path);
            bail!(
                "artifact zip download failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let archive_file = std::fs::File::open(&zip_path)?;
        let mut archive = match zip::ZipArchive::new(archive_file) {
            Ok(archive) => archive,
            Err(err) => {
                // docker/build-push-action uploads `.dockerbuild` build-record
                // artifacts as gzip blobs, not zips. A selected-but-non-zip
                // artifact (only reachable on pattern/download-all requests) is
                // skipped with a warning rather than failing the whole step.
                let _ = std::fs::remove_file(&zip_path);
                eprintln!("skipping artifact '{artifact_name}': not a zip archive ({err})");
                continue;
            }
        };
        let mut files = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let Some(path) = entry.enclosed_name() else {
                let _ = std::fs::remove_file(&zip_path);
                bail!("artifact '{artifact_name}' contains an unsafe archive path");
            };
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            files.push((path, content));
        }
        let _ = std::fs::remove_file(&zip_path);
        downloads.push(ResultsArtifactDownload {
            name: artifact_name.to_string(),
            files,
        });
    }
    Ok(downloads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_json_request_reuses_reqwest_transport_shape() {
        use wiremock::{
            matchers::{body_string, header, method, path},
            Mock, MockServer, ResponseTemplate,
        };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/github"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_string(r#"{"hello":"world"}"#))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let result = native_json_request(
            "POST",
            &format!("{}/github", server.uri()),
            "test-token",
            Some(r#"{"hello":"world"}"#.to_string()),
            5,
        )
        .await
        .unwrap();

        assert_eq!(result, (201, r#"{"ok":true}"#.to_string()));
    }

    #[tokio::test]
    async fn curl_json_request_sets_content_type_for_json_bodies() {
        use wiremock::matchers::{body_string, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/github"))
            .and(header("authorization", "Bearer test-token"))
            .and(header("content-type", "application/json"))
            .and(body_string(r#"{"hello":"world"}"#))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let result = curl_json_request(
            "POST",
            &format!("{}/github", server.uri()),
            "test-token",
            Some(r#"{"hello":"world"}"#.to_string()),
            5,
        )
        .await
        .unwrap();

        assert_eq!(result, (201, r#"{"ok":true}"#.to_string()));
    }

    #[tokio::test]
    async fn canceled_curl_kills_child_and_cleans_private_inputs() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("delayed")
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;

        let prefix = format!("velnor-cancel-test-{}", Uuid::new_v4());
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            run_private_curl(
                &prefix,
                "max-time = 30\nconnect-timeout = 2\nsilent\n",
                None,
                &server.uri(),
                false,
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "curl unexpectedly completed before cancellation"
        );

        let temp_dir = std::env::temp_dir().join("velnor-curl");
        let leaked = std::fs::read_dir(temp_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
        assert!(!leaked, "canceled curl left private input files behind");
    }

    #[tokio::test]
    async fn jit_rate_limit_returns_without_waiting_for_reset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runners/generate-jitconfig",
            ))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("retry-after", "3600")
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", "4102444800")
                    .set_body_string(r#"{"message":"rate limited"}"#),
            )
            .mount(&server)
            .await;

        let scope = GitHubScope::parse(&format!("{}/tailrocks", server.uri())).unwrap();
        let request = GitHubJitConfigRequest {
            name: "velnor-test".to_owned(),
            runner_group_id: 1,
            labels: vec!["velnor".to_owned()],
            work_folder: None,
        };
        let started = std::time::Instant::now();
        let error = RegistrationClient::new()
            .unwrap()
            .generate_jit_config(&scope, "test-token", &request)
            .await
            .unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "rate-limit retry waited too long: {error:#}"
        );
        assert_eq!(
            error
                .downcast_ref::<GitHubApiError>()
                .map(|error| error.status),
            Some(403)
        );
    }

    #[test]
    fn runner_delete_204_and_404_are_gone() {
        assert_eq!(
            classify_runner_delete(204, ""),
            Some(RunnerDeleteOutcome::Gone)
        );
        assert_eq!(
            classify_runner_delete(404, r#"{"message":"Not Found"}"#),
            Some(RunnerDeleteOutcome::Gone)
        );
        assert!(!runner_delete_is_busy_conflict(204, ""));
        assert!(!runner_delete_is_busy_conflict(
            404,
            "currently running a job"
        ));
    }

    #[test]
    fn runner_delete_422_busy_is_quarantine_not_gone() {
        let body =
            r#"{"message":"Sorry, the runner is currently running a job. Unable to delete."}"#;
        assert!(runner_delete_is_busy_conflict(422, body));
        assert_eq!(
            classify_runner_delete(422, body),
            Some(RunnerDeleteOutcome::BusyConflict)
        );
        assert_ne!(
            classify_runner_delete(422, body),
            Some(RunnerDeleteOutcome::Gone)
        );
        assert_eq!(classify_runner_delete(500, body), None);
        assert!(!runner_delete_is_busy_conflict(
            422,
            r#"{"message":"validation failed"}"#
        ));
    }

    #[test]
    fn workflow_cancel_url_and_statuses_are_fail_closed_rest() {
        assert_eq!(
            repository_from_actions_run_url(
                "https://api.github.com/repos/jackin-project/jackin/actions/runs/10"
            )
            .as_deref(),
            Some("jackin-project/jackin")
        );
        assert!(classify_workflow_cancel(202));
        assert!(classify_workflow_cancel(409));
        assert!(classify_workflow_cancel(404));
        assert!(!classify_workflow_cancel(500));
        let scope = GitHubScope::parse("https://github.com/jackin-project").unwrap();
        assert_eq!(
            scope
                .workflow_run_cancel_url("jackin-project/jackin", 10)
                .unwrap()
                .as_str(),
            "https://api.github.com/repos/jackin-project/jackin/actions/runs/10/cancel"
        );
    }

    #[test]
    fn artifact_compression_level_zero_uses_zip_stored() {
        let bytes = artifact_zip_bytes(&[("seed.tar.zst".into(), vec![42; 64])], true).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let file = archive.by_index(0).unwrap();
        assert_eq!(file.compression(), zip::CompressionMethod::Stored);
    }

    #[test]
    fn artifact_retention_sets_results_service_expiration() {
        let now = time::OffsetDateTime::parse(
            "2026-07-18T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let request = artifact_create_request("plan", "job", "seed", Some(14), now).unwrap();
        assert_eq!(request["expires_at"], "2026-08-01T00:00:00Z");
    }

    #[test]
    fn results_service_download_lists_signs_and_extracts_artifact_v4() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("dist/output.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"artifact-v4\n").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let signed_url = format!("{base}/signed.zip?credential=secret");
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                requests.push(request_text);
                let (content_type, body) = match index {
                    0 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({
                            "artifacts": [{
                                "name": "release-linux",
                                "workflow_run_backend_id": "plan",
                                "workflow_job_run_backend_id": "producer"
                            }]
                        }))
                        .unwrap(),
                    ),
                    1 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({"signed_url": signed_url})).unwrap(),
                    ),
                    _ => ("application/zip", zip_bytes.clone()),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });

        let downloads = download_artifacts_blocking(
            &base,
            "runtime-token",
            "plan",
            "consumer",
            "release-linux",
            "",
        )
        .unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].name, "release-linux");
        assert_eq!(
            downloads[0].files,
            vec![(
                std::path::PathBuf::from("dist/output.txt"),
                b"artifact-v4\n".to_vec()
            )]
        );
        let requests = server.join().unwrap();
        assert!(requests[0].contains("ArtifactService/ListArtifacts"));
        assert!(requests[1].contains("ArtifactService/GetSignedArtifactURL"));
        assert!(requests[2].starts_with("GET /signed.zip?credential=secret HTTP/1.1"));
    }

    #[test]
    fn results_service_download_filters_before_signing_and_downloading() {
        // Regression: a run containing a docker/build-push-action `.dockerbuild`
        // build-record artifact (gzip, not zip) must not fail an unrelated
        // download-artifact step. The name filter applies BEFORE any artifact
        // is signed or downloaded, so the server must see exactly one
        // ListArtifacts + one GetSignedArtifactURL + one GET.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("dist/output.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"artifact-v4\n").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let signed_url = format!("{base}/signed.zip?credential=secret");
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).to_string());
                let (content_type, body) = match index {
                    0 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({
                            "artifacts": [
                                {
                                    "name": "release-linux",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "producer"
                                },
                                {
                                    "name": ".dockerbuild",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "image"
                                }
                            ]
                        }))
                        .unwrap(),
                    ),
                    1 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({"signed_url": signed_url})).unwrap(),
                    ),
                    _ => ("application/zip", zip_bytes.clone()),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });

        let downloads = download_artifacts_blocking(
            &base,
            "runtime-token",
            "plan",
            "consumer",
            "release-linux",
            "",
        )
        .unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].name, "release-linux");
        let requests = server.join().unwrap();
        // Exactly three requests: the .dockerbuild artifact was never signed
        // or downloaded (pre-fix it was fetched and failed with EOCD).
        assert_eq!(requests.len(), 3);
        assert!(requests[1].contains("ArtifactService/GetSignedArtifactURL"));
        assert!(requests[2].starts_with("GET /signed.zip?credential=secret HTTP/1.1"));
    }

    #[test]
    fn results_service_download_skips_non_zip_artifacts() {
        // Regression: an unfiltered download-all (merge-multiple) in a run with
        // a non-zip `.dockerbuild` build-record artifact skips that artifact
        // with a warning instead of failing with "invalid Zip archive: EOCD".
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("dist/output.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"artifact-v4\n").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        // .dockerbuild build records are gzip blobs, not zips.
        let gzip_bytes = b"\x1f\x8b\x08\x00dockerbuild-record-not-a-zip".to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let signed_url = format!("{base}/signed.bin?credential=secret");
        let server = std::thread::spawn(move || {
            for index in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let (content_type, body) = match index {
                    0 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({
                            "artifacts": [
                                {
                                    "name": "release-linux",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "producer"
                                },
                                {
                                    "name": ".dockerbuild",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "image"
                                }
                            ]
                        }))
                        .unwrap(),
                    ),
                    1 | 3 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({"signed_url": signed_url})).unwrap(),
                    ),
                    2 => ("application/zip", zip_bytes.clone()),
                    _ => ("application/gzip", gzip_bytes.clone()),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });

        let downloads =
            download_artifacts_blocking(&base, "runtime-token", "plan", "consumer", "", "")
                .unwrap();
        server.join().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].name, "release-linux");
        assert_eq!(
            downloads[0].files,
            vec![(
                std::path::PathBuf::from("dist/output.txt"),
                b"artifact-v4\n".to_vec()
            )]
        );
    }

    #[test]
    fn classify_broker_poll_healthy_empty() {
        assert_eq!(classify_broker_poll(204, ""), BrokerPollClass::Empty);
        assert_eq!(classify_broker_poll(200, "  \n"), BrokerPollClass::Empty);
    }

    #[test]
    fn classify_broker_poll_message() {
        assert_eq!(classify_broker_poll(200, "{}"), BrokerPollClass::Message);
    }

    #[test]
    fn classify_broker_poll_expired_session_is_error_not_idle() {
        // The 2026-06-11 zombie-fleet incident: 401 with an empty body was
        // treated as "no message" and idle slots polled a dead session forever.
        assert_eq!(classify_broker_poll(401, ""), BrokerPollClass::Error);
        assert_eq!(classify_broker_poll(403, ""), BrokerPollClass::Error);
        assert_eq!(classify_broker_poll(404, ""), BrokerPollClass::Error);
        assert_eq!(classify_broker_poll(500, "oops"), BrokerPollClass::Error);
        // curl transport failure yields status 0 and must be an error too.
        assert_eq!(classify_broker_poll(0, ""), BrokerPollClass::Error);
    }

    #[test]
    fn classify_broker_poll_errors_for_recovery() {
        assert_eq!(
            classify_broker_poll_error(401),
            BrokerPollErrorClass::Authentication
        );
        assert_eq!(
            classify_broker_poll_error(404),
            BrokerPollErrorClass::MissingSession
        );
        assert_eq!(
            classify_broker_poll_error(409),
            BrokerPollErrorClass::Conflict
        );
        assert_eq!(
            classify_broker_poll_error(429),
            BrokerPollErrorClass::RateLimited
        );
        assert_eq!(
            classify_broker_poll_error(503),
            BrokerPollErrorClass::Server
        );
        assert_eq!(
            classify_broker_poll_error(0),
            BrokerPollErrorClass::Transport
        );
    }

    #[test]
    fn classify_registration_errors_without_confusing_quota_and_permission() {
        assert_eq!(
            classify_registration_error(403, Some(0), None),
            RegistrationErrorClass::Quota
        );
        assert_eq!(
            classify_registration_error(403, Some(42), None),
            RegistrationErrorClass::Permission
        );
        assert_eq!(
            classify_registration_error(403, Some(42), Some(30)),
            RegistrationErrorClass::Quota
        );
        assert_eq!(
            classify_registration_error(404, None, None),
            RegistrationErrorClass::Missing
        );
        assert_eq!(
            classify_registration_error(409, None, None),
            RegistrationErrorClass::Conflict
        );
        assert_eq!(
            classify_registration_error(503, None, None),
            RegistrationErrorClass::Transient
        );
    }

    #[test]
    fn jit_curl_config_has_bounded_transport_timeouts() {
        let config = jit_curl_config("test-agent", "test-pat");
        assert!(config.contains("connect-timeout = 10"));
        assert!(config.contains("max-time = 45"));
        assert!(config.contains("request = POST"));
    }

    #[test]
    fn parse_runner_lookup_missing_runner_is_none() {
        assert!(parse_runner_lookup(404, "{\"message\":\"Not Found\"}")
            .expect("404 is a definite answer")
            .is_none());
    }

    #[test]
    fn parse_runner_lookup_online_runner() {
        let body = r#"{"id":4237,"name":"velnor-fixture-slot-1","status":"online","busy":false}"#;
        let runner = parse_runner_lookup(200, body)
            .expect("parse")
            .expect("runner present");
        assert_eq!(runner.id, Some(4237));
        assert_eq!(runner.status.as_deref(), Some("online"));
        assert_eq!(runner.busy, Some(false));
    }

    #[test]
    fn parse_runner_lookup_api_failure_is_error() {
        assert!(parse_runner_lookup(500, "boom").is_err());
        assert!(parse_runner_lookup(0, "").is_err());
        assert!(parse_runner_lookup(401, "bad credentials").is_err());
    }

    #[test]
    fn completion_retry_classification() {
        // Transport/5xx/curl-0 retry; throttling retries.
        assert!(is_retriable_completion_status(0));
        assert!(is_retriable_completion_status(500));
        assert!(is_retriable_completion_status(502));
        assert!(is_retriable_completion_status(408));
        assert!(is_retriable_completion_status(429));
        // Deterministic 4xx must not retry.
        assert!(!is_retriable_completion_status(400));
        assert!(!is_retriable_completion_status(401));
        assert!(!is_retriable_completion_status(404));
        assert!(!is_retriable_completion_status(409));
        assert!(!is_retriable_completion_status(422));
    }
    use rsa::{pkcs8::DecodePrivateKey, traits::PrivateKeyParts};

    #[test]
    fn hosted_repo_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/donbeave/velnor").unwrap();

        assert!(scope.hosted);
        assert_eq!(scope.api_base_url.as_str(), "https://api.github.com/");
        assert_eq!(
            scope.jit_config_url.as_str(),
            "https://api.github.com/repos/donbeave/velnor/actions/runners/generate-jitconfig"
        );
        assert_eq!(
            scope.runner_url(42).unwrap().as_str(),
            "https://api.github.com/repos/donbeave/velnor/actions/runners/42"
        );
    }

    #[test]
    fn hosted_org_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/ChainArgos").unwrap();

        assert_eq!(
            scope.jit_config_url.as_str(),
            "https://api.github.com/orgs/ChainArgos/actions/runners/generate-jitconfig"
        );
        assert_eq!(scope.kind(), "organization");
        assert_eq!(
            scope.runner_groups_url().unwrap().as_str(),
            "https://api.github.com/orgs/ChainArgos/actions/runner-groups"
        );
        assert_eq!(
            scope.runner_group_url(7).unwrap().as_str(),
            "https://api.github.com/orgs/ChainArgos/actions/runner-groups/7"
        );
        assert_eq!(
            scope.runner_group_repositories_url(7).unwrap().as_str(),
            "https://api.github.com/orgs/ChainArgos/actions/runner-groups/7/repositories"
        );
    }

    #[test]
    fn hosted_enterprise_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/enterprises/acme").unwrap();

        assert_eq!(
            scope.jit_config_url.as_str(),
            "https://api.github.com/enterprises/acme/actions/runners/generate-jitconfig"
        );
        assert_eq!(scope.kind(), "enterprise");
    }

    #[test]
    fn enterprise_server_scope_uses_api_v3() {
        let scope = GitHubScope::parse("https://github.example.com/org/repo").unwrap();

        assert!(!scope.hosted);
        assert_eq!(
            scope.jit_config_url.as_str(),
            "https://github.example.com/api/v3/repos/org/repo/actions/runners/generate-jitconfig"
        );
    }

    #[test]
    fn ghe_scope_preserves_explicit_port() {
        let scope = GitHubScope::parse("http://127.0.0.1:8443/tailrocks").unwrap();
        assert_eq!(
            scope.runners_url().unwrap().as_str(),
            "http://127.0.0.1:8443/api/v3/orgs/tailrocks/actions/runners"
        );
        assert_eq!(
            scope.runner_groups_url().unwrap().as_str(),
            "http://127.0.0.1:8443/api/v3/orgs/tailrocks/actions/runner-groups"
        );
    }

    #[test]
    fn rejects_unknown_scope_depth() {
        let err = GitHubScope::parse("https://github.com/a/b/c").unwrap_err();

        assert!(err.to_string().contains("must point to org"));
    }

    #[test]
    fn task_agent_payload_keeps_runner_labels() {
        let agent = TaskAgent::new(
            "velnor-1",
            vec!["velnor".into(), "hetzner-sentry-ci".into()],
            None,
            false,
        );
        let json = serde_json::to_value(agent).unwrap();

        assert_eq!(json["name"], "velnor-1");
        assert_eq!(json["maxParallelism"], 1);
        assert_eq!(json["labels"][0]["name"], "self-hosted");
        assert_eq!(json["labels"][3]["name"], "velnor");
        assert_eq!(json["labels"][3]["type"], "User");
        assert_eq!(json["labels"][4]["name"], "hetzner-sentry-ci");
    }

    #[test]
    fn task_agent_accepts_lowercase_label_types_from_github() {
        let agent: TaskAgent = serde_json::from_str(
            r#"{
                "id": 1,
                "name": "velnor-1",
                "version": "2.326.0",
                "osDescription": "linux",
                "maxParallelism": 1,
                "ephemeral": false,
                "disableUpdate": true,
                "labels": [
                    { "name": "self-hosted", "type": "system" },
                    { "name": "velnor", "type": "user" }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(agent.labels[0].r#type, LabelType::System);
        assert_eq!(agent.labels[1].r#type, LabelType::User);
    }

    #[test]
    fn task_agent_accepts_nullable_strings_from_github_list() {
        let agents: Vec<TaskAgent> = parse_vss_list(
            serde_json::json!({
                "count": 1,
                "value": [{
                    "id": 7,
                    "name": "velnor-1",
                    "version": null,
                    "osDescription": null,
                    "maxParallelism": 1,
                    "ephemeral": false,
                    "disableUpdate": true,
                    "labels": []
                }]
            }),
            "get agents",
        )
        .unwrap();

        assert_eq!(agents[0].id, Some(7));
        assert_eq!(agents[0].version, "");
        assert_eq!(agents[0].os_description, "");
    }

    #[test]
    fn task_agent_message_accepts_broker_migration_without_message_id() {
        let message: TaskAgentMessage = serde_json::from_str(
            r#"{
                "messageType": "BrokerMigration",
                "body": "{\"brokerBaseUrl\":\"https://broker.actions.githubusercontent.com\"}"
            }"#,
        )
        .unwrap();

        assert_eq!(message.message_id, 0);
        assert_eq!(message.message_type, "BrokerMigration");
    }

    #[test]
    fn broker_session_response_can_omit_agent() {
        let session: TaskAgentSession = serde_json::from_str(
            r#"{
                "sessionId": "session-1",
                "ownerName": "velnor"
            }"#,
        )
        .unwrap();

        assert_eq!(session.session_id.as_deref(), Some("session-1"));
        assert_eq!(session.agent.id, 0);
    }

    #[test]
    fn oauth_client_assertion_lifetime_fits_github_limit() {
        let key_pair = RunnerKeyPair::generate().unwrap();
        let credentials = OAuthJwtCredentials {
            client_id: "client".into(),
            authorization_url: "https://vstoken.actions.githubusercontent.com/token".into(),
            private_key_pem: key_pair.private_key_pem,
        };

        let assertion = build_client_assertion(&credentials).unwrap();
        let mut parts = assertion.split('.');
        let _header = parts.next().unwrap();
        let claims = parts.next().unwrap();
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(claims)
            .unwrap();
        let claims: Value = serde_json::from_slice(&claims).unwrap();

        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["nbf"].as_u64().unwrap(),
            300
        );
    }

    #[test]
    fn classifies_only_missing_registration_oauth_errors() {
        assert!(oauth_registration_not_found("invalid_client"));
        assert!(oauth_registration_not_found("INVALID_CLIENT"));
        assert!(!oauth_registration_not_found("temporarily_unavailable"));
    }

    #[test]
    fn decodes_jit_config_file_map() {
        let key_pair = RunnerKeyPair::generate().unwrap();
        let private_key = RsaPrivateKey::from_pkcs8_pem(&key_pair.private_key_pem).unwrap();
        let primes = private_key.primes();
        let rsa_params = serde_json::json!({
            "d": STANDARD.encode(private_key.d().to_bytes_be()),
            "exponent": STANDARD.encode(private_key.e().to_bytes_be()),
            "modulus": STANDARD.encode(private_key.n().to_bytes_be()),
            "p": STANDARD.encode(primes[0].to_bytes_be()),
            "q": STANDARD.encode(primes[1].to_bytes_be())
        });
        let files = BTreeMap::from([
            (
                ".runner".to_string(),
                STANDARD.encode(
                    serde_json::json!({
                        "AgentId": 42,
                        "AgentName": "velnor-jit",
                        "PoolId": 1,
                        "PoolName": "Default",
                        "ServerUrl": "https://pipelines.actions.githubusercontent.com/tenant/",
                        "ServerUrlV2": "https://broker.actions.githubusercontent.com/tenant/",
                        "GitHubUrl": "https://github.com/owner/repo",
                        "UseV2Flow": true,
                        "Ephemeral": true,
                        "DisableUpdate": true
                    })
                    .to_string(),
                ),
            ),
            (
                ".credentials".to_string(),
                STANDARD.encode(
                    serde_json::json!({
                        "Scheme": "OAuth",
                        "Data": {
                            "clientId": "client-id",
                            "authorizationUrl": "https://vstoken.actions.githubusercontent.com/token",
                            "requireFipsCryptography": "false"
                        }
                    })
                    .to_string(),
                ),
            ),
            (
                ".credentials_rsaparams".to_string(),
                STANDARD.encode(rsa_params.to_string()),
            ),
        ]);
        let encoded = STANDARD.encode(serde_json::to_string(&files).unwrap());

        let decoded = decode_jit_config(&encoded).unwrap();

        assert_eq!(decoded.settings.agent_id, Some(42));
        assert_eq!(decoded.settings.agent_name.as_deref(), Some("velnor-jit"));
        assert!(decoded.settings.use_v2_flow);
        assert_eq!(
            decoded.settings.server_url_v2.as_deref(),
            Some("https://broker.actions.githubusercontent.com/tenant/")
        );
        assert_eq!(decoded.credentials.scheme, "OAuth");
        assert_eq!(decoded.credentials.data["clientId"], "client-id");
        assert!(decoded.private_key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn distributed_task_base_preserves_server_path() {
        let url = distributed_task_base_url("https://pipelines.actions.githubusercontent.com/abc")
            .unwrap();

        assert_eq!(
            url.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/_apis/distributedtask/"
        );
    }

    #[test]
    fn parses_wrapped_vss_list() {
        let pools: Vec<TaskAgentPool> = parse_vss_list(
            serde_json::json!({
                "count": 1,
                "value": [
                    { "id": 1, "name": "Default", "isHosted": false, "isInternal": true }
                ]
            }),
            "test",
        )
        .unwrap();

        assert_eq!(pools[0].id, 1);
        assert_eq!(pools[0].name.as_deref(), Some("Default"));
        assert!(pools[0].is_internal);
    }

    #[test]
    fn session_payload_matches_agent_reference_shape() {
        let session = TaskAgentSession::new("host (PID: 1)", 42, "velnor");
        let json = serde_json::to_value(session).unwrap();

        assert_eq!(json["ownerName"], "host (PID: 1)");
        assert_eq!(json["agent"]["id"], 42);
        assert_eq!(json["agent"]["name"], "velnor");
        assert_eq!(json["agent"]["version"], RUNNER_VERSION);
        assert_eq!(json["useFipsEncryption"], false);
    }

    #[test]
    fn agent_request_url_matches_classic_runner_route() {
        let base = distributed_task_base_url("https://pipelines.actions.githubusercontent.com/abc")
            .unwrap();
        let url = agent_request_url(&base, 7, 99).unwrap();

        assert_eq!(
            url.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/_apis/distributedtask/pools/7/jobrequests/99?api-version=5.1-preview.1&lockToken=00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn broker_urls_match_official_v2_routes() {
        let base = slash_url("https://broker.actions.githubusercontent.com/tenant").unwrap();

        assert_eq!(
            broker_session_url(&base).unwrap().as_str(),
            "https://broker.actions.githubusercontent.com/tenant/session"
        );
        let message = broker_message_url(&base, "session-1", RunnerStatus::Busy, true).unwrap();
        assert_eq!(message.path(), "/tenant/message");
        let query = message.query().unwrap();
        assert!(query.contains("sessionId=session-1"));
        assert!(query.contains("status=Busy"));
        assert!(query.contains(&format!("runnerVersion={RUNNER_VERSION}")));
        assert!(query.contains("disableUpdate=true"));
        let ack = broker_acknowledge_url(&base, "session-1", RunnerStatus::Online).unwrap();
        assert_eq!(ack.path(), "/tenant/acknowledge");
        assert!(ack.query().unwrap().contains("status=Online"));
    }

    #[test]
    fn run_service_acquire_url_matches_official_route() {
        let url = run_service_acquire_job_url("https://run.actions.githubusercontent.com/jobs/123")
            .unwrap();

        assert_eq!(
            url.as_str(),
            "https://run.actions.githubusercontent.com/jobs/123/acquirejob"
        );
        assert_eq!(
            run_service_renew_job_url("https://run.actions.githubusercontent.com/jobs/123")
                .unwrap()
                .as_str(),
            "https://run.actions.githubusercontent.com/jobs/123/renewjob"
        );
        assert_eq!(
            run_service_complete_job_url("https://run.actions.githubusercontent.com/jobs/123")
                .unwrap()
                .as_str(),
            "https://run.actions.githubusercontent.com/jobs/123/completejob"
        );
    }

    #[test]
    fn acquire_job_non_retriable_statuses_match_upstream() {
        assert!(is_non_retriable_acquire_status(StatusCode::NOT_FOUND));
        assert!(is_non_retriable_acquire_status(StatusCode::CONFLICT));
        assert!(is_non_retriable_acquire_status(
            StatusCode::UNPROCESSABLE_ENTITY
        ));
        assert!(!is_non_retriable_acquire_status(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!is_non_retriable_acquire_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn acquire_failure_classifies_local_faults_separately_from_transport() {
        let permission = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private curl directory",
        ));
        assert!(!acquire_failure_is_transient(&permission));

        let timeout = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "request timed out",
        ));
        assert!(acquire_failure_is_transient(&timeout));

        let unauthorized = anyhow::Error::from(GitHubApiError {
            status: StatusCode::UNAUTHORIZED.as_u16(),
            action: "acquire".into(),
            body: "invalid token".into(),
            retry_after_seconds: None,
            rate_limit_reset_epoch: None,
            remaining: Some(4999),
        });
        assert!(!acquire_failure_is_transient(&unauthorized));
    }

    #[tokio::test]
    async fn acquire_job_retries_transient_failure_before_parsing_job() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::{matchers::method, Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/run/jobs/123/acquirejob"))
            .respond_with(move |_request: &Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(500).set_body_string("retry later")
                } else {
                    ResponseTemplate::new(200)
                        .set_body_string(r#"{"plan":{"planId":"plan-1"},"jobId":"job-1"}"#)
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let run_service = RunServiceClient::new("token")
            .unwrap()
            .with_acquire_retry_delay_for_test(Duration::ZERO);
        let outcome = run_service
            .acquire_job(
                &format!("{}/run/jobs/123", server.uri()),
                "broker-message",
                std::env::consts::OS,
                None,
            )
            .await
            .unwrap();

        let AcquireJobOutcome::Acquired(job) = outcome else {
            panic!("transient acquire failure must be retried");
        };
        assert_eq!(job["jobId"], "job-1");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn acquire_job_rejects_malformed_success_without_retrying_or_swallowing() {
        use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .expect(1)
            .mount(&server)
            .await;

        let run_service = RunServiceClient::new("token")
            .unwrap()
            .with_acquire_retry_delay_for_test(Duration::ZERO);
        let error = run_service
            .acquire_job(
                &format!("{}/run/jobs/123", server.uri()),
                "broker-message",
                std::env::consts::OS,
                None,
            )
            .await
            .expect_err("malformed success must be a permanent acquire error");

        assert!(!is_transient_acquire_error(&error));
        assert!(error.to_string().contains("parse acquire run-service job"));
    }

    #[test]
    fn runner_job_request_ref_accepts_snake_case_broker_body() {
        let reference: RunnerJobRequestRef = serde_json::from_value(serde_json::json!({
            "id": "broker-message",
            "runner_request_id": "request-1",
            "should_acknowledge": true,
            "run_service_url": "https://run.actions.githubusercontent.com/jobs/123/",
            "billing_owner_id": "42"
        }))
        .unwrap();

        assert_eq!(reference.runner_request_id, "request-1");
        assert!(reference.should_acknowledge);
        assert_eq!(
            reference.run_service_url.as_deref(),
            Some("https://run.actions.githubusercontent.com/jobs/123/")
        );
        assert_eq!(reference.billing_owner_id.as_deref(), Some("42"));
    }

    #[test]
    fn acquire_job_request_matches_run_service_shape() {
        let body = serde_json::to_value(AcquireJobRequest {
            job_message_id: "request-1",
            runner_os: "linux",
            billing_owner_id: Some("42"),
        })
        .unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "jobMessageId": "request-1",
                "runnerOS": "linux",
                "billingOwnerId": "42"
            })
        );
    }

    #[test]
    fn complete_job_request_matches_run_service_shape() {
        let completion = RunServiceCompleteJob {
            plan_id: "plan".into(),
            job_id: "job".into(),
            conclusion: TaskResult::Succeeded,
            outputs: [(
                "artifact".into(),
                RunServiceVariableValue {
                    value: "123".into(),
                    is_secret: false,
                },
            )]
            .into(),
            step_results: vec![RunServiceStepResult {
                external_id: Some("step".into()),
                number: None,
                name: "step".into(),
                status: TimelineRecordState::Completed,
                started_at: None,
                completed_at: None,
                conclusion: TaskResult::Succeeded,
                completed_log_lines: 2,
                annotations: vec![RunServiceAnnotation {
                    level: RunServiceAnnotationLevel::Failure,
                    message: "bad config".into(),
                    title: Some("lint".into()),
                    path: Some("src/main.rs".into()),
                    start_line: Some(10),
                    end_line: Some(12),
                    start_column: Some(2),
                    end_column: Some(4),
                    step_number: None,
                    is_infrastructure_issue: false,
                }],
            }],
            annotations: Vec::new(),
            telemetry: vec![RunServiceTelemetry {
                message: "DeprecatedCommand: set-output".into(),
                kind: "ActionCommand".into(),
            }],
            environment_url: Some("https://example.com/env".into()),
            billing_owner_id: Some("42".into()),
            infrastructure_failure_category: Some("runner_bootstrap".into()),
        };

        assert_eq!(
            serde_json::to_value(completion).unwrap(),
            serde_json::json!({
                "planId": "plan",
                "jobId": "job",
                "conclusion": "succeeded",
                "outputs": {
                    "artifact": { "value": "123", "isSecret": false }
                },
                "stepResults": [{
                    "external_id": "step",
                    "name": "step",
                    "status": "completed",
                    "conclusion": "succeeded",
                    "completed_log_lines": 2,
                    "annotations": [{
                        "level": "failure",
                        "message": "bad config",
                        "title": "lint",
                        "path": "src/main.rs",
                        "startLine": 10,
                        "endLine": 12,
                        "startColumn": 2,
                        "endColumn": 4,
                        "isInfrastructureIssue": false
                    }]
                }],
                "telemetry": [{
                    "message": "DeprecatedCommand: set-output",
                    "type": "ActionCommand"
                }],
                "environmentUrl": "https://example.com/env",
                "billingOwnerId": "42",
                "infrastructureFailureCategory": "runner_bootstrap"
            })
        );
    }

    #[test]
    fn server_root_preserves_server_path() {
        let url = server_root_url("https://pipelines.actions.githubusercontent.com/abc").unwrap();

        assert_eq!(
            url.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/"
        );
    }

    #[test]
    fn timeline_routes_match_task_client_shape() {
        let root = server_root_url("https://pipelines.actions.githubusercontent.com/abc").unwrap();
        let records = timeline_records_url(&root, "scope", "build", "plan", "timeline").unwrap();
        let feed = timeline_record_feed_url(&root, "scope", "build", "plan", "timeline", "record")
            .unwrap();
        let logs = timeline_logs_url(&root, "scope", "build", "plan").unwrap();
        let events = plan_events_url(&root, "scope", "build", "plan").unwrap();

        assert_eq!(
            records.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/scope/_apis/distributedtask/hubs/build/plans/plan/timelines/timeline/records?api-version=5.1-preview.1"
        );
        assert_eq!(
            feed.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/scope/_apis/distributedtask/hubs/build/plans/plan/timelines/timeline/records/record/feed?api-version=5.1-preview.1"
        );
        assert_eq!(
            logs.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/scope/_apis/distributedtask/hubs/build/plans/plan/logs?api-version=5.1-preview.1"
        );
        assert_eq!(
            events.as_str(),
            "https://pipelines.actions.githubusercontent.com/abc/scope/_apis/distributedtask/hubs/build/plans/plan/events?api-version=5.1-preview.1"
        );
    }

    #[test]
    fn agent_request_bodies_match_runner_update_shape() {
        let renew = serde_json::to_value(TaskAgentJobRequest::renew(99)).unwrap();
        let finish = serde_json::to_value(TaskAgentJobRequest::finish(
            99,
            "2026-05-31T12:00:00Z",
            TaskResult::Succeeded,
        ))
        .unwrap();

        assert_eq!(renew, serde_json::json!({ "requestId": 99 }));
        assert_eq!(
            finish,
            serde_json::json!({
                "requestId": 99,
                "finishTime": "2026-05-31T12:00:00Z",
                "result": "succeeded"
            })
        );
    }

    #[test]
    fn timeline_record_body_matches_job_record_shape() {
        let record =
            TimelineRecord::job_pending("job-id", "check", Some("build".to_string()), "velnor-1")
                .in_progress("2026-05-31T12:00:00Z")
                .completed("2026-05-31T12:01:00Z", TaskResult::Succeeded);
        let json = serde_json::to_value(record).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "id": "job-id",
                "type": "Job",
                "name": "check",
                "startTime": "2026-05-31T12:00:00Z",
                "finishTime": "2026-05-31T12:01:00Z",
                "percentComplete": 100,
                "state": "completed",
                "result": "succeeded",
                "workerName": "velnor-1",
                "refName": "build",
                "errorCount": 0,
                "warningCount": 0,
                "noticeCount": 0
            })
        );
    }

    #[test]
    fn timeline_record_body_matches_task_record_shape() {
        let record = TimelineRecord::task_completed(
            "step-id",
            "job-id",
            "Build",
            1,
            "2026-05-31T12:01:00Z",
            TaskResult::Failed,
        )
        .with_issue_counts(1, 2, 3);
        let json = serde_json::to_value(record).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "id": "step-id",
                "parentId": "job-id",
                "type": "Task",
                "name": "Build",
                "finishTime": "2026-05-31T12:01:00Z",
                "percentComplete": 100,
                "state": "completed",
                "result": "failed",
                "order": 1,
                "errorCount": 1,
                "warningCount": 2,
                "noticeCount": 3
            })
        );
    }

    #[test]
    fn timeline_record_body_matches_in_progress_task_record_shape() {
        let record = TimelineRecord::task_pending("step-id", "job-id", "Build", 1)
            .in_progress("2026-05-31T12:00:00Z");
        let json = serde_json::to_value(record).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "id": "step-id",
                "parentId": "job-id",
                "type": "Task",
                "name": "Build",
                "startTime": "2026-05-31T12:00:00Z",
                "percentComplete": 0,
                "state": "inProgress",
                "order": 1,
                "errorCount": 0,
                "warningCount": 0,
                "noticeCount": 0
            })
        );
    }

    #[test]
    fn timeline_record_feed_body_matches_runner_shape() {
        let feed = TimelineRecordFeedLines::new("step-id", vec!["hello".to_string()], Some(1));
        let json = serde_json::to_value(feed).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "stepId": "step-id",
                "value": ["hello"],
                "startLine": 1
            })
        );
    }

    #[test]
    fn job_completed_event_body_matches_runner_shape() {
        let event = JobCompletedEvent::new(
            99,
            "job-id",
            TaskResult::Succeeded,
            [("answer".to_string(), "42".to_string())].into(),
        );
        let json = serde_json::to_value(event).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "name": "JobCompleted",
                "jobId": "job-id",
                "requestId": 99,
                "result": "succeeded",
                "outputs": {
                    "answer": {
                        "value": "42",
                        "isSecret": false
                    }
                }
            })
        );
    }

    #[test]
    fn task_agent_job_request_accepts_pascal_response() {
        let request: TaskAgentJobRequest = serde_json::from_str(
            r#"{
                "RequestId": 99,
                "LockedUntil": "2026-05-31T12:05:00Z",
                "Result": "Succeeded",
                "JobName": "check"
            }"#,
        )
        .unwrap();

        assert_eq!(request.request_id, 99);
        assert_eq!(
            request.locked_until.as_deref(),
            Some("2026-05-31T12:05:00Z")
        );
        assert!(matches!(request.result, Some(TaskResult::Succeeded)));
        assert_eq!(request.job_name.as_deref(), Some("check"));
    }

    #[test]
    fn builds_rs256_oauth_client_assertion() {
        let key_pair = RunnerKeyPair::generate().unwrap();
        let credentials = OAuthJwtCredentials {
            client_id: "client-id".to_string(),
            authorization_url: "https://vstoken.actions.githubusercontent.com/token".to_string(),
            private_key_pem: key_pair.private_key_pem,
        };

        let jwt = build_client_assertion(&credentials).unwrap();
        let parts: Vec<_> = jwt.split('.').collect();

        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|part| !part.is_empty()));
    }

    #[test]
    fn decoded_jit_runner_settings_accepts_string_agent_id() {
        // GitHub returns AgentId/PoolId as quoted strings in some JIT payloads.
        let json = r#"{"AgentId":"23","PoolId":"1"}"#;
        let settings: DecodedJitRunnerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.agent_id, Some(23));
        assert_eq!(settings.pool_id, Some(1));
    }

    #[test]
    fn decoded_jit_runner_settings_accepts_integer_agent_id() {
        let json = r#"{"AgentId":42,"PoolId":7}"#;
        let settings: DecodedJitRunnerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.agent_id, Some(42));
        assert_eq!(settings.pool_id, Some(7));
    }

    #[test]
    fn decoded_jit_runner_settings_accepts_string_booleans() {
        // GitHub returns UseV2Flow and Ephemeral as capitalized strings.
        let json = r#"{"UseV2Flow":"True","Ephemeral":"True","DisableUpdate":"False"}"#;
        let settings: DecodedJitRunnerSettings = serde_json::from_str(json).unwrap();
        assert!(settings.use_v2_flow);
        assert!(settings.ephemeral);
        assert!(!settings.disable_update);
    }

    #[test]
    fn decoded_jit_runner_settings_accepts_native_booleans() {
        let json = r#"{"UseV2Flow":true,"Ephemeral":false}"#;
        let settings: DecodedJitRunnerSettings = serde_json::from_str(json).unwrap();
        assert!(settings.use_v2_flow);
        assert!(!settings.ephemeral);
    }

    #[test]
    fn github_retry_headers_drive_reset_aware_delay() {
        let hint = parse_github_retry_headers(
            b"HTTP/2 403\r\nRetry-After: 17\r\nX-RateLimit-Reset: 1060\r\nX-RateLimit-Remaining: 0\r\n\r\n",
        );
        assert_eq!(hint.retry_after_seconds, Some(17));
        assert_eq!(hint.rate_limit_reset_epoch, Some(1060));
        assert_eq!(hint.remaining, Some(0));
        assert_eq!(hint.delay(1000), Some(std::time::Duration::from_secs(60)));

        let error = github_api_error_with_retry("quota", 403, "exhausted", hint);
        assert!(github_api_retry_delay(&error).is_some());
        assert_eq!(
            github_api_quota_status(&error).and_then(|status| status.remaining),
            Some(0)
        );
    }

    #[test]
    fn github_api_quota_status_requires_exhaustion_not_permission() {
        let permission = github_api_error_with_retry(
            "JIT runner config request",
            403,
            "Resource not accessible by integration",
            GitHubRetryHint {
                retry_after_seconds: None,
                rate_limit_reset_epoch: Some(1_800_000_000),
                remaining: Some(4200),
            },
        );
        assert!(
            github_api_quota_status(&permission).is_none(),
            "permission 403 with remaining>0 must not fleet-hold"
        );
        assert!(
            github_api_retry_delay(&permission).is_some(),
            "reset headers may still delay the failing slot"
        );

        let exhausted = github_api_error_with_retry(
            "JIT runner config request",
            403,
            "API rate limit exceeded",
            GitHubRetryHint {
                retry_after_seconds: None,
                rate_limit_reset_epoch: Some(1_800_000_000),
                remaining: Some(0),
            },
        );
        let quota = github_api_quota_status(&exhausted).expect("quota 403 remaining=0");
        assert_eq!(quota.remaining, Some(0));
        assert_eq!(quota.rate_limit_reset_epoch, Some(1_800_000_000));

        let throttled = github_api_error_with_retry(
            "JIT runner config request",
            429,
            "too many requests",
            GitHubRetryHint::default(),
        );
        assert!(github_api_quota_status(&throttled).is_some());
    }

    #[test]
    fn reqwest_header_map_preserves_github_retry_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("29"));
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("123456"));
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("4999"));

        assert_eq!(
            github_retry_hint_from_header_map(&headers),
            GitHubRetryHint {
                retry_after_seconds: Some(29),
                rate_limit_reset_epoch: Some(123456),
                remaining: Some(4999),
            }
        );
    }

    #[test]
    fn rate_limit_status_requires_real_exhaustion_evidence() {
        // Permission 403s also carry x-ratelimit headers with a non-zero
        // remaining count; those must NOT be classified as rate limits.
        let permission = GitHubRateLimitStatus {
            retry_after_seconds: None,
            rate_limit_reset_epoch: Some(1_800_000_000),
            remaining: Some(4200),
        };
        assert!(!permission.is_limited(403));

        let exhausted = GitHubRateLimitStatus {
            retry_after_seconds: None,
            rate_limit_reset_epoch: Some(1_800_000_000),
            remaining: Some(0),
        };
        assert!(exhausted.is_limited(403));
        assert!(!exhausted.is_limited(200));

        let abuse = GitHubRateLimitStatus {
            retry_after_seconds: Some(30),
            rate_limit_reset_epoch: None,
            remaining: Some(4200),
        };
        assert!(abuse.is_limited(403));
        assert_eq!(abuse.reset_epoch_or_retry_after(1_000), Some(1_030));
        assert_eq!(
            exhausted.reset_epoch_or_retry_after(1_000),
            Some(1_800_000_000)
        );

        let throttled = GitHubRateLimitStatus::default();
        assert!(throttled.is_limited(429));
    }

    #[test]
    fn malformed_retry_headers_are_ignored_without_exposing_values() {
        let hint = parse_github_retry_headers(
            b"Retry-After: later\r\nX-RateLimit-Reset: invalid\r\nAuthorization: secret\r\n",
        );
        assert_eq!(hint, GitHubRetryHint::default());
        assert_eq!(hint.delay(1000), None);
    }
}
