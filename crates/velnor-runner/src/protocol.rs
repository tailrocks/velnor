#![allow(dead_code)]

use crate::runner_json::{
    clr_big_integer_to_f64, dotnet_double_general, is_clr_uri, non_finite_text,
    parse_clr_double_text, parse_json_text, JsonNumber, JsonNumberKind, JsonReaderOrigin,
    OrderedJsonValue,
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, USER_AGENT},
    Client, Method, StatusCode,
};
use rsa::{
    pkcs8::{EncodePrivateKey, LineEnding},
    rand_core::{OsRng, RngCore},
    traits::PublicKeyParts,
    BigUint, RsaPrivateKey,
};
use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt,
    io::{Read, Seek, Write},
    process::{Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;
use uuid::Uuid;
use velnor_model::{ContextValue, NonFinite};

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
const GITHUB_CONNECT_TIMEOUT_SECS: u64 = 2;
const GITHUB_MAX_TIME_SECS: u64 = 5;
const GITHUB_CONTENTS_MAX_TIME_SECS: u64 = 30;
const GITHUB_CURL_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const OAUTH_MAX_RESPONSE_BYTES: usize = 64 * 1024;
const RUN_SERVICE_ACQUIRE_MAX_ATTEMPTS: u32 = 5;
const RUN_SERVICE_ACQUIRE_RETRY_MIN_MS: u64 = 5_000;
const RUN_SERVICE_ACQUIRE_RETRY_MAX_MS: u64 = 15_000;
const RESULTS_ARTIFACT_MAX_DOWNLOAD_RESPONSE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_ZIP_MEMBERS: usize = 100_000;
const RESULTS_ARTIFACT_MAX_ZIP_PATH_BYTES: u64 = 64 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_ZIP_PATH_DEPTH: usize = 256;
const RESULTS_ARTIFACT_MAX_ZIP_CENTRAL_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_RAW_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_TOTAL_RETURNED_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_LISTED_ARTIFACTS: usize = 100_000;
const RESULTS_ARTIFACT_MAX_UPLOAD_FILES: usize = 100_000;
const RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES: u64 = 64 * 1024 * 1024;

const RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH: usize = 256;
const RESULTS_ARTIFACT_MAX_UPLOAD_SOURCE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_UPLOAD_ZIP_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const ARTIFACT_TRANSFER_MIN_BYTES_PER_SECOND: u64 = 4 * 1024 * 1024;
const ARTIFACT_TRANSFER_GRACE_SECONDS: u64 = 120;
const ARTIFACT_TRANSFER_MAX_SECONDS: u64 = 60 * 60;

/// Sample uniformly from `0..upper_bound`, using rejection to avoid modulo
/// bias in the run-service retry jitter.
fn random_u64_below(upper_bound: u64) -> u64 {
    let threshold = upper_bound.wrapping_neg() % upper_bound;
    loop {
        let value = OsRng.next_u64();
        if value >= threshold {
            return value % upper_bound;
        }
    }
}

/// Retry category of a broker/completion boundary failure, decided once at
/// the boundary where the status and body are observed. Retry, abandon, and
/// refresh policy matches on this category; it never re-parses error text.
/// (GOAL 31, same shape as `DockerErrorCategory`.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokerErrorCategory {
    /// Transport break, 5xx, 408/429: the same request may succeed later.
    /// Retry with backoff inside the path's bounded attempt budget.
    Transient,
    /// Deterministic refusal classified as terminal by its owning boundary
    /// (for example, a typed acquire 404 or non-retriable session-create 4xx).
    /// Fail fast — no retry — and, on the completion path, spend the durable
    /// attempt budget at once so the slot is released now instead of after
    /// hours of doomed retries.
    Terminal,
    /// Velnor's ownership-safety category: typed acquire 409/422 and session-
    /// create 409 retain the provisional intent for the `renewjob` oracle.
    /// actions/runner only says acquire 409/422 are non-retriable; retaining a
    /// 422 intent is a separate local safety policy, not upstream behavior.
    Conflict,
}

/// Typed GitHub/run-service API failure. `Display` is the historical
/// boundary message (`{action} failed: status={status}, body={body}`); the
/// broker/completion boundary additionally attaches a [`BrokerErrorCategory`]
/// on this type (rather than in a wrapper) so the error chain keeps its
/// shape and downstream `GitHubApiError` downcasts (credential refresh,
/// quota/rate-limit hints) keep working untouched.
#[derive(thiserror::Error)]
#[error("{action} failed: status={status}, body={body}")]
pub struct GitHubApiError {
    pub status: u16,
    pub action: String,
    pub(crate) body: String,
    pub retry_after_seconds: Option<u64>,
    pub rate_limit_reset_epoch: Option<u64>,
    /// `x-ratelimit-remaining`. Required to tell quota 403 (remaining=0)
    /// from permission 403 (remaining>0); GitHub sends reset headers on both.
    pub remaining: Option<u64>,
    /// Boundary-produced [`BrokerErrorCategory`]. `Some` only at the classified
    /// broker/completion production sites (acquire attempt, completion
    /// refusal and exhausted-transient budget, broker acknowledge, broker
    /// session create): transport failures carry no status to classify from,
    /// and every other `GitHubApiError` producer predates the taxonomy (see
    /// the remaining-untyped-sites list in `plans/2026-09-14-r0-798-corr.md`).
    pub(crate) category: Option<BrokerErrorCategory>,
}

impl fmt::Debug for GitHubApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubApiError")
            .field("status", &self.status)
            .field("action", &self.action)
            .field("body", &"<redacted>")
            .field("retry_after_seconds", &self.retry_after_seconds)
            .field("rate_limit_reset_epoch", &self.rate_limit_reset_epoch)
            .field("remaining", &self.remaining)
            .field("category", &self.category)
            .finish()
    }
}

/// Boundary-produced category of a broker/completion failure, or `None`
/// when the boundary never classified it (transport errors, other
/// producers, test doubles). Callers must define the unclassified default
/// explicitly at the decision site: the completion journal fails open
/// (retry) because a finished job's outcome must never be lost to one
/// unrecognized error — the reverse of the Docker boundary's fail-closed
/// default, where the cost asymmetry runs the other way.
pub(crate) fn broker_error_category(error: &anyhow::Error) -> Option<BrokerErrorCategory> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<GitHubApiError>()
            .and_then(|api| api.category)
    })
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

/// Busy-runner vocabulary for a 422 runner DELETE. GitHub sends no machine
/// code for this case — the known production message is `Sorry, the runner
/// is currently running a job. Unable to delete.` — so the vocabulary match
/// is scoped to the envelope's parsed message fields, never to the raw body:
/// a repository name, URL, or unrelated field that happens to contain these
/// words must not fake a conflict.
const RUNNER_BUSY_MESSAGE_NEEDLES: &[&str] = &[
    "currently running a job",
    "unable to delete",
    "runner is busy",
    "runner_is_busy",
];

pub(crate) fn runner_delete_is_busy_conflict(status: u16, body: &str) -> bool {
    if status != 422 {
        return false;
    }
    runner_delete_messages(body).iter().any(|message| {
        let lower = message.to_ascii_lowercase();
        RUNNER_BUSY_MESSAGE_NEEDLES
            .iter()
            .any(|needle| lower.contains(needle))
    })
}

/// Parsed `message` fields of a GitHub error envelope: the top-level
/// `message` plus every `errors[]` entry message. Empty when the body is not
/// a JSON envelope — an unparseable 422 is a generic API error, never a
/// proven conflict (fail closed; the unclassified path surfaces
/// `GitHubApiError`).
fn runner_delete_messages(body: &str) -> Vec<String> {
    let Ok(envelope) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    if let Some(message) = envelope.get("message").and_then(Value::as_str) {
        messages.push(message.to_string());
    }
    if let Some(errors) = envelope.get("errors").and_then(Value::as_array) {
        for entry in errors {
            if let Some(message) = entry.as_str() {
                messages.push(message.to_string());
            } else if let Some(message) = entry.get("message").and_then(Value::as_str) {
                messages.push(message.to_string());
            }
        }
    }
    messages
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

/// Keep provider-controlled response text useful without allowing credentials
/// or unbounded payloads into errors, forensic logs, or controller output.
fn sanitize_response_body(raw: &str) -> String {
    const MAX_RESPONSE_BODY_BYTES: usize = 4096;
    const SENSITIVE_MARKERS: &[&str] = &[
        "authorization:",
        "bearer ",
        "access_token",
        "access-token",
        "client_secret",
        "client-secret",
        "password",
        "private_key",
        "private-key",
        "secret",
        "signature=",
        "sig=",
        "token",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "begin private key",
    ];
    let lower = raw.to_ascii_lowercase();
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return "<redacted response body>".to_owned();
    }

    let mut output = String::with_capacity(raw.len().min(MAX_RESPONSE_BODY_BYTES));
    let mut truncated = false;
    for character in raw.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > MAX_RESPONSE_BODY_BYTES {
            truncated = true;
            break;
        }
        output.push(character);
    }
    if truncated {
        output.push('…');
    }
    output
}

fn github_api_error(
    action: impl Into<String>,
    status: u16,
    body: impl Into<String>,
) -> anyhow::Error {
    GitHubApiError {
        status,
        action: action.into(),
        body: sanitize_response_body(&body.into()),
        retry_after_seconds: None,
        rate_limit_reset_epoch: None,
        remaining: None,
        category: None,
    }
    .into()
}

/// Broker/completion-boundary failure with the boundary-produced
/// [`BrokerErrorCategory`] attached. Same message as [`github_api_error`].
fn github_api_error_categorized(
    action: impl Into<String>,
    status: u16,
    body: impl Into<String>,
    category: BrokerErrorCategory,
) -> anyhow::Error {
    GitHubApiError {
        status,
        action: action.into(),
        body: sanitize_response_body(&body.into()),
        retry_after_seconds: None,
        rate_limit_reset_epoch: None,
        remaining: None,
        category: Some(category),
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
        let seconds = self
            .rate_limit_reset_epoch
            .map(|reset| reset.saturating_sub(now_epoch))
            .unwrap_or_else(|| self.retry_after_seconds.unwrap_or(0));
        (seconds > 0).then(|| std::time::Duration::from_secs(seconds))
    }
}

/// Validate an endpoint before sending an authenticated request.
///
/// GitHub response data and job messages can supply URLs. HTTPS is mandatory
/// for remote endpoints so bearer tokens, PATs, and OAuth assertions cannot
/// be sent in cleartext. Loopback HTTP is available only to unit tests and the
/// explicit non-default integration-test feature; release daemons never send
/// credentials over cleartext HTTP.
pub(crate) fn validate_authenticated_url(raw: &str) -> Result<Url> {
    let safe = redacted_authenticated_url(raw);
    let url = Url::parse(raw).with_context(|| format!("parse authenticated URL '{safe}'"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("authenticated URL must not contain userinfo");
    }
    match url.scheme() {
        "https" => Ok(url),
        "http"
            if cfg!(feature = "test-support") && url.host_str().is_some_and(is_loopback_host) =>
        {
            Ok(url)
        }
        _ => bail!("authenticated endpoint must use HTTPS: {safe}"),
    }
}

/// GitHub publishes hosted Actions services on fixed and regional
/// subdomains below this suffix. Keep the match to one valid DNS label so a
/// lookalike domain or a nested untrusted host cannot pass the allowlist.
pub(crate) fn is_github_actions_service_host(host: &str) -> bool {
    const SUFFIX: &str = ".actions.githubusercontent.com";

    let Some(prefix_len) = host.len().checked_sub(SUFFIX.len()) else {
        return false;
    };
    let (prefix, suffix) = host.split_at(prefix_len);
    if !suffix.eq_ignore_ascii_case(SUFFIX) || prefix.is_empty() {
        return false;
    }

    let bytes = prefix.as_bytes();
    bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn validate_known_service_url(raw: &str, field: &str, hosts: &[&str]) -> Result<Url> {
    let url = validate_authenticated_url(raw)?;
    let host = url.host_str().unwrap_or_default();
    let allowed_host = is_loopback_host(host)
        || hosts.iter().any(|allowed| {
            host.eq_ignore_ascii_case(allowed)
                || (is_github_actions_service_host(host) && is_github_actions_service_host(allowed))
        });
    let allowed_port = is_loopback_host(host) || url.port().is_none();
    if !allowed_host || !allowed_port {
        bail!(
            "{field} endpoint host is not an approved GitHub Actions service: {}",
            redacted_authenticated_url(raw)
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{field} endpoint must not contain query or fragment data");
    }
    Ok(url)
}

fn validate_signed_blob_url(raw: &str, field: &str) -> Result<Url> {
    validate_authenticated_url(raw).with_context(|| format!("validate {field} signed blob URL"))
}

pub(crate) fn redacted_authenticated_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return "<invalid URL>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// Render a reqwest failure without including its URL. Reqwest's Display and
/// source chain may contain a signed blob URL, so authenticated callers must
/// never put the original error in a context or log message.
pub(crate) fn redacted_reqwest_error(error: &reqwest::Error) -> String {
    if let Some(status) = error.status() {
        return format!("HTTP status {status}");
    }
    if error.is_timeout() {
        return "request timed out".to_owned();
    }
    if error.is_redirect() {
        return "request redirect failed".to_owned();
    }
    if error.is_body() {
        return "request body transfer failed".to_owned();
    }
    if error.is_decode() {
        return "response decoding failed".to_owned();
    }
    if error.is_builder() {
        return "request construction failed".to_owned();
    }
    "request transport failed".to_owned()
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
}

pub(crate) fn unix_epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_github_retry_headers(headers: &[u8], response_epoch: u64) -> GitHubRetryHint {
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
    if let Some(retry_after) = hint.retry_after_seconds {
        let retry_until = response_epoch.saturating_add(retry_after);
        hint.rate_limit_reset_epoch =
            Some(hint.rate_limit_reset_epoch.unwrap_or(0).max(retry_until));
    }
    hint
}

fn github_retry_hint_from_header_map(headers: &HeaderMap, response_epoch: u64) -> GitHubRetryHint {
    let parse = |name: &'static str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    };
    let mut hint = GitHubRetryHint {
        retry_after_seconds: parse("retry-after"),
        rate_limit_reset_epoch: parse("x-ratelimit-reset"),
        remaining: parse("x-ratelimit-remaining"),
    };
    if let Some(retry_after) = hint.retry_after_seconds {
        let retry_until = response_epoch.saturating_add(retry_after);
        hint.rate_limit_reset_epoch =
            Some(hint.rate_limit_reset_epoch.unwrap_or(0).max(retry_until));
    }
    hint
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
        body: sanitize_response_body(&body.into()),
        retry_after_seconds: hint.retry_after_seconds,
        rate_limit_reset_epoch: hint.rate_limit_reset_epoch,
        remaining: hint.remaining,
        category: None,
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
    github_api_retry_delay_at(error, unix_epoch_now())
}

fn github_api_retry_delay_at(error: &anyhow::Error, now_epoch: u64) -> Option<std::time::Duration> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .and_then(|error| {
            let status = error.rate_limit_status();
            let retry_after = status
                .retry_after_seconds
                .map(std::time::Duration::from_secs);
            let quota_reset = status
                .is_limited(error.status)
                .then(|| {
                    status.reset_epoch_or_retry_after(now_epoch).map(|reset| {
                        std::time::Duration::from_secs(reset.saturating_sub(now_epoch))
                    })
                })
                .flatten();
            match (retry_after, quota_reset) {
                (Some(retry_after), Some(quota_reset)) => Some(retry_after.max(quota_reset)),
                (Some(retry_after), None) => Some(retry_after),
                (None, Some(quota_reset)) => Some(quota_reset),
                (None, None) => None,
            }
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

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubScope {
    pub original_url: String,
    pub hosted: bool,
    pub api_base_url: Url,
    pub jit_config_url: Url,
    runner_scope_path: String,
}

impl fmt::Debug for GitHubScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubScope")
            .field(
                "original_url",
                &redacted_authenticated_url(&self.original_url),
            )
            .field("hosted", &self.hosted)
            .field(
                "api_base_url",
                &redacted_authenticated_url(self.api_base_url.as_str()),
            )
            .field(
                "jit_config_url",
                &redacted_authenticated_url(self.jit_config_url.as_str()),
            )
            .field("runner_scope_path", &self.runner_scope_path)
            .finish()
    }
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

#[derive(Clone, Deserialize)]
pub struct GitHubJitConfigResponse {
    pub runner: GitHubJitRunner,
    pub encoded_jit_config: String,
}

impl fmt::Debug for GitHubJitConfigResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubJitConfigResponse")
            .field("runner", &self.runner)
            .field("encoded_jit_config", &"<redacted>")
            .finish()
    }
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
    /// GitHub's JIT response currently omits this summary field. The
    /// encoded `.runner` file is the authoritative ephemeral assertion.
    #[serde(default)]
    pub ephemeral: Option<bool>,
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

#[derive(Clone, PartialEq, Eq)]
pub struct DecodedJitConfig {
    pub settings: DecodedJitRunnerSettings,
    pub credentials: DecodedJitCredentials,
    pub private_key_pem: String,
}

impl fmt::Debug for DecodedJitConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedJitConfig")
            .field("settings", &self.settings)
            .field("credentials", &"<redacted>")
            .field("private_key_pem", &"<redacted>")
            .finish()
    }
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

#[derive(Clone, PartialEq, Eq, Deserialize)]
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

impl fmt::Debug for DecodedJitRunnerSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let server_url = self.server_url.as_deref().map(redacted_authenticated_url);
        let server_url_v2 = self
            .server_url_v2
            .as_deref()
            .map(redacted_authenticated_url);
        let github_url = self.github_url.as_deref().map(redacted_authenticated_url);
        f.debug_struct("DecodedJitRunnerSettings")
            .field("agent_id", &self.agent_id)
            .field("agent_name", &self.agent_name)
            .field("pool_id", &self.pool_id)
            .field("pool_name", &self.pool_name)
            .field("server_url", &server_url)
            .field("server_url_v2", &server_url_v2)
            .field("github_url", &github_url)
            .field("work_folder", &self.work_folder)
            .field("use_v2_flow", &self.use_v2_flow)
            .field("ephemeral", &self.ephemeral)
            .field("disable_update", &self.disable_update)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct DecodedJitCredentials {
    #[serde(rename = "Scheme", alias = "scheme")]
    pub scheme: String,
    #[serde(rename = "Data", alias = "data")]
    pub data: BTreeMap<String, String>,
}

impl fmt::Debug for DecodedJitCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedJitCredentials")
            .field("scheme", &self.scheme)
            .field("data", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct OAuthJwtCredentials {
    pub client_id: String,
    pub authorization_url: String,
    pub private_key_pem: String,
}

impl fmt::Debug for OAuthJwtCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthJwtCredentials")
            .field("client_id", &self.client_id)
            .field(
                "authorization_url",
                &redacted_authenticated_url(&self.authorization_url),
            )
            .field("private_key_pem", &"<redacted>")
            .finish()
    }
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

#[derive(Clone, Serialize, Deserialize)]
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

impl fmt::Debug for OAuthTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokenResponse")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("error", &self.error)
            .field("error_description", &self.error_description)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthAccessToken {
    pub token: String,
    pub expires_in: Option<std::time::Duration>,
}

impl fmt::Debug for OAuthAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthAccessToken")
            .field("token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
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
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build OAuth HTTP client")?;
        Ok(Self { http })
    }

    pub async fn exchange_client_credentials(
        &self,
        credentials: &OAuthJwtCredentials,
    ) -> Result<OAuthAccessToken> {
        let transport = github_http_transport()?;
        validate_authenticated_url(&credentials.authorization_url)?;
        let assertion = build_client_assertion(credentials)?;
        // Build the URL-encoded OAuth form body.
        let body: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair(
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            )
            .append_pair("client_assertion", &assertion)
            .finish();
        let url = credentials.authorization_url.clone();
        let (status, text) = match transport {
            "native" => {
                let response = native_http_client()?
                    .post(&url)
                    .header(USER_AGENT, RUNNER_USER_AGENT)
                    .header(ACCEPT, "application/json")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .timeout(Duration::from_secs(GITHUB_MAX_TIME_SECS))
                    .body(body.clone())
                    .send()
                    .await
                    .context("send OAuth token request")?;
                let status = response.status().as_u16();
                let text = response
                    .text()
                    .await
                    .context("read OAuth token response body")?;
                (status, text)
            }
            "curl" => {
                let response = curl_oauth_form_request(&url, &body, GITHUB_MAX_TIME_SECS)
                    .await
                    .context("send OAuth token request")?;
                (response.status, response.body)
            }
            other => bail!("OAuth HTTP transport selector returned an unknown value: {other}"),
        };

        parse_oauth_token_response(status, &text)
    }
}

fn parse_oauth_token_response(status: u16, body: &str) -> Result<OAuthAccessToken> {
    if body.len() > OAUTH_MAX_RESPONSE_BYTES {
        bail!("OAuth token response exceeded {OAUTH_MAX_RESPONSE_BYTES} bytes");
    }
    if !(200..300).contains(&status) && status != StatusCode::BAD_REQUEST.as_u16() {
        // Do not attach the response body: an unexpected body could contain a
        // token and would then be copied into daemon error logs.
        bail!("OAuth token request failed: HTTP status {status}");
    }

    let token_response: OAuthTokenResponse =
        serde_json::from_str(body.trim()).context("parse OAuth token response")?;

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

#[derive(Clone)]
pub struct RunnerKeyPair {
    pub private_key_pem: String,
    pub public_key: TaskAgentPublicKey,
}

impl fmt::Debug for RunnerKeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerKeyPair")
            .field("private_key_pem", &"<redacted>")
            .field("public_key", &self.public_key)
            .finish()
    }
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
pub struct RegistrationClient;

pub(crate) struct GithubHttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    pub(crate) headers: HeaderMap,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GithubContentsRequestError {
    /// Sending a request or reading its response stream failed.
    Transport(String),
    /// The response body exceeds the caller's bounded-read limit.
    /// `status` is the HTTP status when the caller already read it; `None`
    /// when the envelope was too large to parse a status line.
    BodyTooLarge {
        max_body_bytes: usize,
        status: Option<u16>,
    },
    /// The response body is not UTF-8, so the caller cannot parse its text format.
    BodyInvalidUtf8 { status: Option<u16> },
    /// Local configuration or response framing failed before a usable response existed.
    Internal(String),
}

impl fmt::Display for GithubContentsRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(detail) => {
                write!(formatter, "GitHub Contents transport failed: {detail}")
            }
            Self::BodyTooLarge { max_body_bytes, .. } => write!(
                formatter,
                "GitHub Contents response body exceeds {max_body_bytes} bytes"
            ),
            Self::BodyInvalidUtf8 { .. } => {
                formatter.write_str("GitHub Contents response body is not valid UTF-8")
            }
            Self::Internal(detail) => {
                write!(
                    formatter,
                    "GitHub Contents request failed internally: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for GithubContentsRequestError {}

/// Read an authenticated raw GitHub Contents response through the selected
/// host transport. Native mode keeps the existing blocking client; curl mode
/// uses the typed argv/header-pipe path and never falls back to native.
pub(crate) fn github_contents_request(
    client: &reqwest::blocking::Client,
    url: &str,
    bearer_token: &str,
    max_body_bytes: usize,
) -> std::result::Result<GithubHttpResponse, GithubContentsRequestError> {
    let transport = github_http_transport()
        .map_err(|error| GithubContentsRequestError::Internal(format!("{error:#}")))?;
    validate_authenticated_url(url)
        .map_err(|error| GithubContentsRequestError::Internal(format!("{error:#}")))?;
    match transport {
        "native" => {
            let response = client
                .get(url)
                .bearer_auth(bearer_token)
                .header(ACCEPT, "application/vnd.github.raw+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .timeout(Duration::from_secs(GITHUB_CONTENTS_MAX_TIME_SECS))
                .send()
                .map_err(|error| {
                    GithubContentsRequestError::Transport(format!(
                        "send native GitHub Contents request {}: {error}",
                        redacted_authenticated_url(url)
                    ))
                })?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let content_length = response.content_length();
            let body = read_bounded_http_body(response, content_length, max_body_bytes, status)?;
            Ok(GithubHttpResponse {
                status,
                body,
                headers,
            })
        }
        "curl" => {
            let redacted_url = redacted_authenticated_url(url);
            let spec = curl_command_args(
                "GET",
                url,
                bearer_token,
                None,
                GITHUB_CONTENTS_MAX_TIME_SECS,
                "application/vnd.github.raw+json",
                Some("2026-03-10"),
            )
            .map_err(|error| {
                GithubContentsRequestError::Internal(format!(
                    "build curl GitHub Contents request {redacted_url}: {error:#}"
                ))
            })?;
            run_curl_contents_command(spec, max_body_bytes)
        }
        other => Err(GithubContentsRequestError::Internal(format!(
            "github HTTP transport selector returned an unknown value: {other}"
        ))),
    }
}

pub(crate) fn read_bounded_http_body<R: Read>(
    reader: R,
    content_length: Option<u64>,
    max_body_bytes: usize,
    status: u16,
) -> std::result::Result<String, GithubContentsRequestError> {
    let max_body_bytes_u64 = u64::try_from(max_body_bytes).map_err(|error| {
        GithubContentsRequestError::Internal(format!("metadata body limit overflows: {error}"))
    })?;
    if content_length.is_some_and(|length| length > max_body_bytes_u64) {
        return Err(GithubContentsRequestError::BodyTooLarge {
            max_body_bytes,
            status: Some(status),
        });
    }
    let mut body =
        Vec::with_capacity(content_length.unwrap_or_default().min(max_body_bytes_u64) as usize);
    reader
        .take(max_body_bytes_u64.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|error| {
            GithubContentsRequestError::Transport(format!(
                "read GitHub Contents response body: {error}"
            ))
        })?;
    if body.len() > max_body_bytes {
        return Err(GithubContentsRequestError::BodyTooLarge {
            max_body_bytes,
            status: Some(status),
        });
    }
    String::from_utf8(body).map_err(|_| GithubContentsRequestError::BodyInvalidUtf8 {
        status: Some(status),
    })
}

fn github_error_from_response(action: &str, response: GithubHttpResponse) -> anyhow::Error {
    github_api_error_with_retry(
        action,
        response.status,
        response.body,
        github_retry_hint_from_header_map(&response.headers, unix_epoch_now()),
    )
}

fn github_transport_error(action: &str, error: impl fmt::Display) -> anyhow::Error {
    github_api_error(action, 0, error.to_string())
}

fn github_json_body<T>(action: &str, response: GithubHttpResponse) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    if !(200..300).contains(&response.status) {
        return Err(github_error_from_response(action, response));
    }
    serde_json::from_str(&response.body).map_err(|error| {
        github_api_error_with_retry(
            action,
            response.status,
            format!("{}; parse error: {error}", response.body),
            github_retry_hint_from_header_map(&response.headers, unix_epoch_now()),
        )
    })
}

async fn github_http_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<GithubHttpResponse> {
    let transport = github_http_transport()?;
    validate_authenticated_url(url)?;
    if transport == "curl" {
        return curl_http_request(
            method,
            url,
            bearer_token,
            json_body,
            max_time_secs,
            "application/vnd.github+json",
            Some("2026-03-10"),
        )
        .await;
    }
    let method_name = method.to_owned();
    let method = Method::from_bytes(method.as_bytes()).map_err(|error| {
        github_transport_error(&format!("parse GitHub HTTP method '{method}'"), error)
    })?;
    let client = native_http_client()
        .map_err(|error| github_transport_error("build GitHub HTTP client", error))?;
    let mut request = client
        .request(method, url)
        .bearer_auth(bearer_token)
        .header(USER_AGENT, RUNNER_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .timeout(Duration::from_secs(max_time_secs));
    if let Some(body) = json_body {
        request = request
            .header("Content-Type", "application/json")
            .body(body);
    }
    let response = request.send().await.map_err(|error| {
        github_transport_error(
            &format!(
                "send native GitHub request {method_name} {}",
                redacted_authenticated_url(url)
            ),
            error,
        )
    })?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.text().await.map_err(|error| {
        github_transport_error(
            &format!(
                "read native GitHub response body {method_name} {}",
                redacted_authenticated_url(url)
            ),
            error,
        )
    })?;
    Ok(GithubHttpResponse {
        status,
        body,
        headers,
    })
}

struct CurlCommandSpec {
    args: Vec<OsString>,
    header_stdin: Vec<u8>,
}

fn curl_command_args(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<&str>,
    max_time_secs: u64,
    accept: &str,
    api_version: Option<&str>,
) -> Result<CurlCommandSpec> {
    Method::from_bytes(method.as_bytes())
        .with_context(|| format!("parse GitHub HTTP method '{method}'"))?;

    let max_time_secs = max_time_secs.max(1);
    let connect_timeout_secs = GITHUB_CONNECT_TIMEOUT_SECS.min(max_time_secs);
    let mut header_stdin = Vec::new();
    append_curl_header(
        &mut header_stdin,
        "Authorization",
        &format!("Bearer {bearer_token}"),
    )?;
    append_curl_header(&mut header_stdin, "User-Agent", RUNNER_USER_AGENT)?;
    append_curl_header(&mut header_stdin, "Accept", accept)?;
    if let Some(api_version) = api_version {
        append_curl_header(&mut header_stdin, "X-GitHub-Api-Version", api_version)?;
    }
    if json_body.is_some() {
        append_curl_header(&mut header_stdin, "Content-Type", "application/json")?;
    }

    let mut args = vec![
        OsString::from("--disable"),
        OsString::from("--silent"),
        OsString::from("--show-error"),
        OsString::from("--request"),
        OsString::from(method),
        OsString::from("--url"),
        OsString::from(url),
        OsString::from("--connect-timeout"),
        OsString::from(connect_timeout_secs.to_string()),
        OsString::from("--max-time"),
        OsString::from(max_time_secs.to_string()),
        // Curl has no implicit retries, but make the mutation safety policy
        // visible and stable if its defaults ever change.
        OsString::from("--retry"),
        OsString::from("0"),
        OsString::from("--dump-header"),
        OsString::from("-"),
        // Feed all headers through stdin so the bearer token is not present
        // in the child argv or in any command-line diagnostic.
        OsString::from("--header"),
        OsString::from("@-"),
    ];
    if let Some(json_body) = json_body {
        // --data-raw keeps a JSON body beginning with `@` as data, not a
        // filename. It is a distinct argv element; no shell is involved.
        args.push(OsString::from("--data-raw"));
        args.push(OsString::from(json_body));
    }

    Ok(CurlCommandSpec { args, header_stdin })
}

fn append_curl_header(headers: &mut Vec<u8>, name: &str, value: &str) -> Result<()> {
    let value = HeaderValue::from_str(value).context("build curl GitHub request header")?;
    headers.extend_from_slice(name.as_bytes());
    headers.extend_from_slice(b": ");
    headers.extend_from_slice(value.as_bytes());
    headers.push(b'\n');
    Ok(())
}

async fn curl_http_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
    accept: &str,
    api_version: Option<&str>,
) -> Result<GithubHttpResponse> {
    validate_authenticated_url(url)?;
    let method_name = method.to_owned();
    let redacted_url = redacted_authenticated_url(url);
    let spec = curl_command_args(
        method,
        url,
        bearer_token,
        json_body.as_deref(),
        max_time_secs,
        accept,
        api_version,
    )
    .map_err(|error| {
        github_transport_error(
            &format!("build curl GitHub request {method_name} {redacted_url}"),
            error,
        )
    })?;
    tokio::task::spawn_blocking(move || run_curl_command(spec))
        .await
        .context("join curl GitHub request")?
        .map_err(|error| {
            github_transport_error(
                &format!("send curl GitHub request {method_name} {redacted_url}"),
                error,
            )
        })
}

async fn curl_oauth_form_request(
    url: &str,
    body: &str,
    max_time_secs: u64,
) -> Result<GithubHttpResponse> {
    validate_authenticated_url(url)?;
    let max_time_secs = max_time_secs.max(1);
    let connect_timeout_secs = GITHUB_CONNECT_TIMEOUT_SECS.min(max_time_secs);
    let redacted_url = redacted_authenticated_url(url);
    let args = vec![
        OsString::from("--disable"),
        OsString::from("--silent"),
        OsString::from("--show-error"),
        OsString::from("--request"),
        OsString::from("POST"),
        OsString::from("--url"),
        OsString::from(url),
        OsString::from("--connect-timeout"),
        OsString::from(connect_timeout_secs.to_string()),
        OsString::from("--max-time"),
        OsString::from(max_time_secs.to_string()),
        // Curl has no implicit retries, but make the mutation safety policy
        // visible and stable if its defaults ever change.
        OsString::from("--retry"),
        OsString::from("0"),
        OsString::from("--dump-header"),
        OsString::from("-"),
        OsString::from("--header"),
        OsString::from(format!("User-Agent: {RUNNER_USER_AGENT}")),
        OsString::from("--header"),
        OsString::from("Accept: application/json"),
        OsString::from("--header"),
        OsString::from("Content-Type: application/x-www-form-urlencoded"),
        // Keep the OAuth assertion out of argv and process diagnostics.
        OsString::from("--data-binary"),
        OsString::from("@-"),
    ];
    let body = body.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || run_curl_oauth_form_command(args, body))
        .await
        .context("join curl OAuth token request")?
        .map_err(|error| {
            github_transport_error(
                &format!("send curl OAuth token request {redacted_url}"),
                error,
            )
        })
}

fn run_curl_oauth_form_command(args: Vec<OsString>, body: Vec<u8>) -> Result<GithubHttpResponse> {
    let mut child = Command::new("curl")
        .args(args)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn curl OAuth token request")?;

    let mut body_stdin = child
        .stdin
        .take()
        .context("open curl OAuth token request body pipe")?;
    if let Err(error) = body_stdin.write_all(&body) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("write curl OAuth token request body");
    }
    drop(body_stdin);

    let output = child
        .wait_with_output()
        .context("wait for curl OAuth token request")?;
    if !output.status.success() {
        let exit = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        bail!("curl OAuth token request exited with status {exit}");
    }
    if output.stdout.len() > OAUTH_MAX_RESPONSE_BYTES {
        bail!("curl OAuth token response exceeded {OAUTH_MAX_RESPONSE_BYTES} bytes");
    }
    parse_curl_response(&output.stdout)
}

fn run_curl_command(spec: CurlCommandSpec) -> Result<GithubHttpResponse> {
    let output = run_curl_command_output(spec).map_err(anyhow::Error::new)?;
    parse_curl_response(&output)
}

fn run_curl_contents_command(
    spec: CurlCommandSpec,
    max_body_bytes: usize,
) -> std::result::Result<GithubHttpResponse, GithubContentsRequestError> {
    let output = run_curl_command_output(spec)?;
    parse_curl_response_with_body_limit(&output, Some(max_body_bytes))
}

fn run_curl_command_output(
    spec: CurlCommandSpec,
) -> std::result::Result<Vec<u8>, GithubContentsRequestError> {
    let mut child = Command::new("curl")
        .args(spec.args)
        // The curl child does not need the operator token in its environment;
        // it receives the in-memory value through the private header pipe.
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            GithubContentsRequestError::Internal(format!("spawn curl GitHub request: {error}"))
        })?;

    let mut header_stdin = child.stdin.take().ok_or_else(|| {
        GithubContentsRequestError::Internal(
            "open curl GitHub request header pipe: stdin pipe is unavailable".to_string(),
        )
    })?;
    if let Err(error) = header_stdin.write_all(&spec.header_stdin) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GithubContentsRequestError::Transport(format!(
            "write curl GitHub request headers: {error}"
        )));
    }
    drop(header_stdin);

    let output = child.wait_with_output().map_err(|error| {
        GithubContentsRequestError::Transport(format!("wait for curl GitHub request: {error}"))
    })?;
    if !output.status.success() {
        let exit = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        return Err(GithubContentsRequestError::Transport(format!(
            "curl GitHub request exited with status {exit}"
        )));
    }
    Ok(output.stdout)
}

fn parse_curl_response(output: &[u8]) -> Result<GithubHttpResponse> {
    parse_curl_response_with_body_limit(output, None).map_err(anyhow::Error::new)
}

fn parse_curl_response_with_body_limit(
    output: &[u8],
    max_body_bytes: Option<usize>,
) -> std::result::Result<GithubHttpResponse, GithubContentsRequestError> {
    if output.len() > GITHUB_CURL_MAX_RESPONSE_BYTES {
        return Err(match max_body_bytes {
            Some(max_body_bytes) => GithubContentsRequestError::BodyTooLarge {
                max_body_bytes,
                status: None,
            },
            None => GithubContentsRequestError::Internal(format!(
                "curl GitHub response exceeded {GITHUB_CURL_MAX_RESPONSE_BYTES} bytes"
            )),
        });
    }

    let mut offset = 0;
    let (status, headers) = loop {
        let remaining = output.get(offset..).ok_or_else(|| {
            GithubContentsRequestError::Internal(
                "curl GitHub response ended before headers".to_string(),
            )
        })?;
        if !remaining.starts_with(b"HTTP/") {
            return Err(GithubContentsRequestError::Internal(
                "curl GitHub response is missing an HTTP status line".to_string(),
            ));
        }
        let (header_end, separator_len) = curl_header_terminator(remaining).ok_or_else(|| {
            GithubContentsRequestError::Internal(
                "curl GitHub response headers are not terminated".to_string(),
            )
        })?;
        let (status, headers) =
            parse_curl_header_block(&remaining[..header_end]).map_err(|error| {
                GithubContentsRequestError::Internal(format!(
                    "parse curl GitHub response headers: {error:#}"
                ))
            })?;
        offset = offset
            .saturating_add(header_end)
            .saturating_add(separator_len);
        if (100..200).contains(&status) {
            continue;
        }
        break (status, headers);
    };

    let body = output.get(offset..).ok_or_else(|| {
        GithubContentsRequestError::Internal(
            "curl GitHub response body offset is invalid".to_string(),
        )
    })?;
    if let Some(max_body_bytes) = max_body_bytes
        && body.len() > max_body_bytes
    {
        return Err(GithubContentsRequestError::BodyTooLarge {
            max_body_bytes,
            status: Some(status),
        });
    }
    let body = String::from_utf8(body.to_vec()).map_err(|_| {
        GithubContentsRequestError::BodyInvalidUtf8 {
            status: Some(status),
        }
    })?;
    Ok(GithubHttpResponse {
        status,
        body,
        headers,
    })
}

fn curl_header_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((index, 4));
    }
    bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2))
}

fn parse_curl_header_block(block: &[u8]) -> Result<(u16, HeaderMap)> {
    let mut lines = block.split(|byte| *byte == b'\n');
    let status_line = lines.next().context("curl response has no status line")?;
    let status_line = status_line.strip_suffix(b"\r").unwrap_or(status_line);
    let status_line = std::str::from_utf8(status_line).context("curl status line is not UTF-8")?;
    let mut fields = status_line.split_ascii_whitespace();
    let version = fields.next().context("curl status line has no version")?;
    if !version.starts_with("HTTP/") {
        bail!("curl response status line has invalid protocol");
    }
    let status = fields
        .next()
        .context("curl status line has no status code")?
        .parse::<u16>()
        .context("parse curl response status code")?;

    let mut headers = HeaderMap::new();
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .context("curl response header has no colon")?;
        let name =
            HeaderName::from_bytes(&line[..colon]).context("parse curl response header name")?;
        let value = HeaderValue::from_bytes(trim_ascii_bytes(&line[colon + 1..]))
            .context("parse curl response header value")?;
        headers.append(name, value);
    }
    Ok((status, headers))
}

fn trim_ascii_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

impl RegistrationClient {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub async fn generate_jit_config(
        &self,
        scope: &GitHubScope,
        pat: &str,
        request: &GitHubJitConfigRequest,
    ) -> Result<GitHubJitConfigResponse> {
        let url = scope.jit_config_url.to_string();
        let body = serde_json::to_string(request).context("serialize JIT config request")?;
        let response = github_http_request("POST", &url, pat, Some(body), GITHUB_MAX_TIME_SECS)
            .await
            .context("send JIT runner config request")?;
        if response.status == 201 {
            return github_json_body("parse JIT runner config response", response);
        }
        // A JIT POST can succeed remotely even when its response is lost or
        // arrives as 5xx. Never issue an automatic second POST: the pending
        // marker and outer supervisor retain ownership of recovery.
        Err(github_error_from_response(
            "JIT runner config request",
            response,
        ))
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
        const PAGE_SIZE: usize = 100;
        const MAX_PAGES: u32 = 100;
        const MAX_ITEMS: usize = PAGE_SIZE * MAX_PAGES as usize;
        let base = scope.runners_url()?;
        let mut all = Vec::new();
        let mut page_number = 1u32;
        loop {
            if page_number > MAX_PAGES || all.len() >= MAX_ITEMS {
                bail!("runner listing exceeded bounded response limit ({MAX_ITEMS} runners)");
            }
            let mut url = base.clone();
            url.query_pairs_mut()
                .append_pair("per_page", &PAGE_SIZE.to_string())
                .append_pair("page", &page_number.to_string());
            let page: Page = github_json_body(
                "list runners response",
                github_http_request("GET", url.as_str(), pat, None, 30).await?,
            )?;
            let fetched = page.runners.len();
            if all.len().saturating_add(fetched) > MAX_ITEMS {
                bail!("runner listing exceeded bounded response limit ({MAX_ITEMS} runners)");
            }
            all.extend(page.runners);
            let total = page.total_count.unwrap_or(all.len() as u64);
            if total > MAX_ITEMS as u64 {
                bail!("runner listing exceeds bounded response limit ({MAX_ITEMS} runners)");
            }
            if fetched < 100 || all.len() as u64 >= total {
                return Ok(all);
            }
            page_number += 1;
        }
    }

    pub async fn find_runner_group(
        &self,
        scope: &GitHubScope,
        pat: &str,
        requested_name: &str,
        requested_id: Option<i64>,
    ) -> Result<RunnerGroup> {
        #[derive(Deserialize)]
        struct Page {
            total_count: u64,
            runner_groups: Vec<RunnerGroup>,
        }
        const PAGE_SIZE: usize = 100;
        const MAX_PAGES: u32 = 100;
        const MAX_ITEMS: usize = PAGE_SIZE * MAX_PAGES as usize;
        let base = scope.runner_groups_url()?;
        if requested_name.is_empty() && requested_id.is_none() {
            bail!("runner group lookup requires a name or numeric id");
        }
        if let Some(requested_id) = requested_id {
            let url = scope.runner_group_url(requested_id)?;
            let group: RunnerGroup = github_json_body(
                "get runner group response",
                github_http_request("GET", url.as_str(), pat, None, 30).await?,
            )?;
            if !requested_name.is_empty() && !group.name.eq_ignore_ascii_case(requested_name) {
                bail!(
                    "runner group '{}' resolves to id {}, not supplied --pool-id {}",
                    group.name,
                    group.id,
                    requested_id
                );
            }
            if group.id != requested_id {
                bail!(
                    "runner group response identity mismatch: requested id {requested_id}, returned {}",
                    group.id
                );
            }
            return Ok(group);
        }
        let mut page_number = 1u32;
        let mut items_seen = 0usize;
        loop {
            if page_number > MAX_PAGES || items_seen >= MAX_ITEMS {
                bail!("runner group lookup exceeded bounded response limit ({MAX_ITEMS} groups)");
            }
            let mut url = base.clone();
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page_number.to_string());
            let page: Page = github_json_body(
                "list runner groups response",
                github_http_request("GET", url.as_str(), pat, None, 30).await?,
            )?;
            let fetched = page.runner_groups.len();
            items_seen = items_seen.saturating_add(fetched);
            if page.total_count > MAX_ITEMS as u64 || items_seen as u64 > page.total_count {
                bail!(
                    "runner group response has inconsistent total_count {} after {} items",
                    page.total_count,
                    items_seen
                );
            }
            if let Some(group) = page.runner_groups.into_iter().find(|group| {
                group.name.eq_ignore_ascii_case(requested_name)
                    || requested_id.is_some_and(|id| group.id == id)
            }) {
                if !requested_name.is_empty() && !group.name.eq_ignore_ascii_case(requested_name) {
                    bail!(
                        "runner group '{}' resolves to id {}, not supplied --pool-id {}",
                        requested_name,
                        group.id,
                        requested_id.unwrap_or_default()
                    );
                }
                return Ok(group);
            }
            if fetched < PAGE_SIZE || items_seen as u64 >= page.total_count {
                if items_seen as u64 != page.total_count {
                    bail!(
                        "runner group pagination ended at {} of {} items",
                        items_seen,
                        page.total_count
                    );
                }
                let identity = requested_id
                    .map(|id| format!("id {id}"))
                    .unwrap_or_else(|| format!("name '{requested_name}'"));
                bail!("runner group {identity} not found")
            }
            page_number += 1;
        }
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
                Err(error) => return Err(error),
            };
            for run_id in runs {
                match self
                    .list_workflow_run_jobs(scope, pat, &repository, run_id)
                    .await
                {
                    Ok(run_jobs) => jobs.extend(run_jobs),
                    Err(error) => return Err(error),
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
            let page: Vec<Repo> = github_json_body(
                "list org repositories response",
                github_http_request("GET", url.as_str(), pat, None, 30)
                    .await
                    .with_context(|| format!("list repositories for org {org}"))?,
            )?;
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
        let runs: Runs = github_json_body(
            "list queued workflow runs response",
            github_http_request("GET", url.as_str(), pat, None, 30)
                .await
                .with_context(|| format!("list queued runs for {repository}"))?,
        )?;
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
        let jobs: Jobs = github_json_body(
            "list workflow run jobs response",
            github_http_request("GET", url.as_str(), pat, None, 30)
                .await
                .with_context(|| format!("list jobs for {repository} run {run_id}"))?,
        )?;
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
        let response = github_http_request("POST", url.as_str(), pat, None, 30).await?;
        if classify_workflow_cancel(response.status) {
            return Ok(());
        }
        Err(github_error_from_response("cancel workflow run", response))
    }

    /// Look up one runner registration by id. `Ok(None)` means GitHub no
    /// longer knows the runner (404). The transport is selected by
    /// `VELNOR_GITHUB_HTTP_TRANSPORT`; the native path uses pooled requests.
    pub async fn get_runner(
        &self,
        scope: &GitHubScope,
        pat: &str,
        runner_id: i64,
    ) -> Result<Option<ListedRunner>> {
        let url = scope.runner_url(runner_id)?;
        parse_runner_lookup_response(
            github_http_request("GET", url.as_str(), pat, None, 30).await?,
            runner_id,
        )
    }

    pub async fn delete_runner(
        &self,
        scope: &GitHubScope,
        pat: &str,
        runner_id: i64,
    ) -> Result<()> {
        let url = scope.runner_url(runner_id)?;
        let response = github_http_request("DELETE", url.as_str(), pat, None, 30).await?;
        match classify_runner_delete(response.status, &response.body) {
            Some(RunnerDeleteOutcome::Gone) => Ok(()),
            Some(RunnerDeleteOutcome::BusyConflict) => {
                Err(RunnerBusyConflict("GitHub reported the runner is busy".into()).into())
            }
            None => Err(github_error_from_response(
                "delete runner request",
                response,
            )),
        }
    }
}

/// Transport selector for GitHub REST requests (api.github.com).
///
/// The selector is explicit so a missing or unsupported configuration cannot
/// silently choose a legacy transport. `native` is the normal path; `curl`
/// runs the host's `curl` for macOS hosts whose outbound network filter
/// (Little Snitch and similar) holds TCP connects from a binary it has no
/// rule for — the connect times out with no TLS or DNS involved, while the
/// system `curl` the filter already trusts goes through. TLS is not the
/// cause. Only REST goes through this selector: the broker, run-service,
/// results-service, and log/blob uploads stay in-process, so a filtered
/// host registers under `curl` yet still loses those (best-effort) uploads
/// until the filter allows `velnor-runner` itself.
pub const GITHUB_HTTP_TRANSPORT_ENV: &str = "VELNOR_GITHUB_HTTP_TRANSPORT";

fn parse_github_http_transport(configured: &str) -> Result<&'static str> {
    match configured.trim() {
        "native" => Ok("native"),
        "curl" => Ok("curl"),
        value => bail!(
            "unsupported {GITHUB_HTTP_TRANSPORT_ENV} value '{value}'; accepted values: native, curl"
        ),
    }
}

pub fn github_http_transport() -> Result<&'static str> {
    let configured = std::env::var(GITHUB_HTTP_TRANSPORT_ENV).with_context(|| {
        format!("{GITHUB_HTTP_TRANSPORT_ENV} must be set to 'native' or 'curl'")
    })?;
    parse_github_http_transport(&configured)
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
    let response =
        github_json_http_response(method, url, bearer_token, json_body, max_time_secs).await?;
    Ok((response.status, response.body))
}

/// Request form used by run-service acquisition, which must inspect the
/// response Content-Type just as actions/runner's typed JSON client does.
async fn github_json_http_response(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<GithubHttpResponse> {
    match github_http_transport()? {
        "native" => {
            native_json_http_response(method, url, bearer_token, json_body, max_time_secs).await
        }
        "curl" => {
            curl_http_request(
                method,
                url,
                bearer_token,
                json_body,
                max_time_secs,
                "application/json",
                None,
            )
            .await
        }
        other => bail!("github HTTP transport selector returned an unknown value: {other}"),
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
    match github_http_transport()? {
        "native" => {
            native_json_request_with_rate_limit(method, url, bearer_token, json_body, max_time_secs)
                .await
        }
        "curl" => {
            let response = curl_http_request(
                method,
                url,
                bearer_token,
                json_body,
                max_time_secs,
                "application/json",
                None,
            )
            .await?;
            let rate_limit =
                github_retry_hint_from_header_map(&response.headers, unix_epoch_now()).into();
            Ok((response.status, response.body, rate_limit))
        }
        other => bail!("github HTTP transport selector returned an unknown value: {other}"),
    }
}

fn native_http_client() -> Result<Client> {
    static CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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

async fn native_json_http_response(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<GithubHttpResponse> {
    validate_authenticated_url(url)?;
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
    let response = request.send().await.with_context(|| {
        format!(
            "send native GitHub request {method_name} {}",
            redacted_authenticated_url(url)
        )
    })?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response
        .text()
        .await
        .context("read native GitHub response body")?;
    Ok(GithubHttpResponse {
        status,
        body,
        headers,
    })
}

async fn native_json_request(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String)> {
    let response =
        native_json_http_response(method, url, bearer_token, json_body, max_time_secs).await?;
    Ok((response.status, response.body))
}

/// Native transport variant that also reports rate-limit telemetry.
async fn native_json_request_with_rate_limit(
    method: &str,
    url: &str,
    bearer_token: &str,
    json_body: Option<String>,
    max_time_secs: u64,
) -> Result<(u16, String, GitHubRateLimitStatus)> {
    validate_authenticated_url(url)?;
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
    let response = request.send().await.with_context(|| {
        format!(
            "send native GitHub request {method_name} {}",
            redacted_authenticated_url(url)
        )
    })?;
    let status = response.status().as_u16();
    let rate_limit = github_retry_hint_from_header_map(response.headers(), unix_epoch_now()).into();
    let body = response
        .text()
        .await
        .context("read native GitHub response body")?;
    Ok((status, body, rate_limit))
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
    /// Transport failure without an HTTP status or non-2xx. An
    /// expired/unauthorized/deleted session typically answers 401/403/404 with
    /// an EMPTY body — this MUST classify as an error, never as "no message",
    /// or an idle slot turns into a zombie that polls forever while GitHub's
    /// scheduler has already dropped the runner (2026-06-11 fleet incident).
    Error,
}

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

pub fn classify_broker_poll_error(status: u16) -> BrokerPollErrorClass {
    match status {
        401 => BrokerPollErrorClass::Authentication,
        403 => BrokerPollErrorClass::Forbidden,
        404 => BrokerPollErrorClass::MissingSession,
        409 => BrokerPollErrorClass::Conflict,
        429 => BrokerPollErrorClass::RateLimited,
        400..=499 => BrokerPollErrorClass::Client,
        500..=599 => BrokerPollErrorClass::Server,
        _ => BrokerPollErrorClass::Transport,
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

/// Completion must retry transport failures, 5xx, and status-less failures; other
/// Deterministic 4xx (auth and validation) will not change on retry. A
/// transient 409 conflict retries like the upstream RunService client. A
/// provider's typed run-service 404 job-not-found response is handled as a
/// separate successful terminal observation by the completion loop.
pub fn is_retriable_completion_status(status: u16) -> bool {
    !(400..500).contains(&status) || matches!(status, 408 | 409 | 429)
}

/// Whether a failed completion send is one that retrying can never fix.
///
/// The completion journal needs this to distinguish a node that is merely
/// unlucky from a payload the run service will refuse forever. A permanent
/// refusal spends the whole recovery budget at once, so the job's slot is
/// released now instead of after hours of doomed retries.
///
/// Transport failures and status-less errors are never permanent: not knowing
/// the remote's answer is the case where retrying is the only correct move.
#[must_use]
pub fn completion_failure_is_permanent(error: &anyhow::Error) -> bool {
    // Category-driven: the completion boundary attaches `Terminal` to a
    // deterministic refusal, and that verdict wins over any re-derivation.
    // Unclassified errors (transport failures, test doubles) fall back to
    // the historical status derivation, which fails open to retry: not
    // knowing the remote's answer is the case where retrying is the only
    // correct move.
    if let Some(category) = broker_error_category(error) {
        return matches!(category, BrokerErrorCategory::Terminal);
    }
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .is_some_and(|api| !is_retriable_completion_status(api.status))
}

/// Protocol disposition for a completion POST.
///
/// `RemoteObservedTerminal` is a successful observation: the remote service
/// already accepted or terminalized the job, so retrying would risk
/// redelivery. The journal-owning caller must persist this disposition as its
/// durable `RemoteObservedTerminal` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionAcknowledgement {
    Accepted,
    RemoteObservedTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionResponseClass {
    Accepted,
    RemoteObservedTerminal,
    RetryableFailure,
    PermanentFailure,
}

fn classify_completion_response(status: u16, body: &str) -> CompletionResponseClass {
    match status {
        200..=299 => CompletionResponseClass::Accepted,
        // The upstream client classifies a failed run-service response from
        // the body's `statusCode`, not the outer HTTP status. Keep 2xx and
        // status-less transport handling ahead of this branch.
        status if status != 0 && is_run_service_job_not_found(body) => {
            CompletionResponseClass::RemoteObservedTerminal
        }
        status if is_retriable_completion_status(status) => {
            CompletionResponseClass::RetryableFailure
        }
        _ => CompletionResponseClass::PermanentFailure,
    }
}

/// Error envelope returned by the run service. Mirrors actions/runner's
/// `RunServiceError` (`src/Sdk/RSWebApi/Contracts/RunServiceError.cs`): the
/// serde names below are the upstream `DataMember(Name = ...)` wire names, not
/// the C# property identifiers. `Code` is the property; `statusCode` is the
/// wire field, and only the wire name may appear here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunServiceError {
    pub(crate) source: Option<String>,
    pub(crate) code: Option<i32>,
    pub(crate) message: Option<String>,
}

impl<'de> Deserialize<'de> for RunServiceError {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RunServiceErrorVisitor;

        impl<'de> Visitor<'de> for RunServiceErrorVisitor {
            type Value = RunServiceError;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a run-service error object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut error = RunServiceError {
                    source: None,
                    code: None,
                    message: None,
                };
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    // Json.NET populates members in wire order, so the last
                    // case-insensitive alias wins. Deserialize every matching
                    // occurrence as it arrives; an invalid earlier value
                    // throws before a later duplicate could replace it.
                    if clr_ordinal_ignore_case_eq(&key, "source") {
                        error.source =
                            clr_json_nullable_string(&value).map_err(serde::de::Error::custom)?;
                    } else if clr_ordinal_ignore_case_eq(&key, "statusCode") {
                        error.code = Some(match &value {
                            Value::Number(number) => {
                                clr_json_int32(number).map_err(serde::de::Error::custom)?
                            }
                            Value::String(value) => value
                                .trim()
                                .parse::<i32>()
                                .map_err(serde::de::Error::custom)?,
                            _ => {
                                return Err(serde::de::Error::custom(
                                    "statusCode must be an Int32",
                                ));
                            }
                        });
                    } else if clr_ordinal_ignore_case_eq(&key, "errorMessage") {
                        error.message =
                            clr_json_nullable_string(&value).map_err(serde::de::Error::custom)?;
                    }
                }
                Ok(error)
            }
        }

        deserializer.deserialize_map(RunServiceErrorVisitor)
    }
}

fn clr_json_nullable_string(
    value: &Value,
) -> std::result::Result<Option<String>, ClrValidationError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        Value::Number(value) => Ok(Some(value.to_string())),
        Value::Bool(value) => Ok(Some(if *value { "True" } else { "False" }.to_owned())),
        _ => Err(clr_reader_error("expected CLR string-compatible value")),
    }
}

impl RunServiceError {
    /// actions/runner's `RunServiceHttpClient.TryParseErrorBody`: a body only
    /// counts as a run-service error when it parses and `source` is exactly
    /// `actions-run-service`. Anything else is an unrelated API failure.
    fn parse(body: &str) -> Option<Self> {
        let error: Self = serde_json::from_str(body).ok()?;
        (error.source.as_deref() == Some(RUN_SERVICE_ERROR_SOURCE)).then_some(error)
    }
}

/// `RunServiceHttpClient.TryParseErrorBody` only accepts this source value.
const RUN_SERVICE_ERROR_SOURCE: &str = "actions-run-service";

/// The typed `statusCode` of a run-service error body, when the body really is
/// one. Upstream first uses outer HTTP status to distinguish success from
/// failure; only a failed response's run-service body can supply a typed error
/// verdict. This avoids treating an unrelated proxy or gateway status as a
/// run-service error.
fn run_service_error_code(body: &str) -> Option<i32> {
    RunServiceError::parse(body).and_then(|error| error.code)
}

/// Match the exact error shape used by actions/runner's RunService client for
/// a missing completion job. A bare 404 or an unrelated API 404 is not proof
/// that this job was already terminal.
fn is_run_service_job_not_found(body: &str) -> bool {
    run_service_error_code(body) == Some(404)
}

/// Typed non-retriable acquire response codes from actions/runner's
/// `RunServiceHttpClient.GetJobMessageAsync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireJobSkipReason {
    /// The run service has no message for this acquire request.
    NotFound,
    /// The run service reports the request was already acquired.
    AlreadyAcquired,
    /// The run service rejected the request as unprocessable.
    Unprocessable,
}

/// Classification of one acquire HTTP response. Upstream treats a 2xx HTTP
/// response as success before inspecting an error envelope. For non-2xx
/// responses, only typed run-service codes 404, 409, and 422 are special
/// non-retriable errors; all other exceptions are retried by `RunServer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquireJobResponseClass {
    Success,
    Skipped(AcquireJobSkipReason),
    RetryableFailure,
}

fn classify_acquire_job_response(http_status: u16, body: &str) -> AcquireJobResponseClass {
    if (200..300).contains(&http_status) {
        return AcquireJobResponseClass::Success;
    }
    if http_status == 0 {
        return AcquireJobResponseClass::RetryableFailure;
    }

    match acquire_job_skip_reason(body) {
        Some(reason) => AcquireJobResponseClass::Skipped(reason),
        None => AcquireJobResponseClass::RetryableFailure,
    }
}

/// Whether actions/runner's `RawHttpClientBase` considers a successful acquire
/// response body JSON. Its `HasContent` check excludes HTTP 204 and a known
/// zero-length body; only the exact `application/json` media type is decoded.
fn is_acquire_job_json_response(
    http_status: u16,
    content_length: Option<u64>,
    content_type: Option<&str>,
) -> bool {
    http_status != StatusCode::NO_CONTENT.as_u16()
        && content_length != Some(0)
        && content_type
            .and_then(|value| value.split(';').next())
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

/// Json.NET populates read-only collections through their lazy getters. Null
/// leaves the backing field unset, and a later getter call creates an empty
/// collection. This applies to collections only; explicit null for a
/// non-nullable CLR value type is a typed deserialization error.
trait ClrCollection<'de>: Deserialize<'de> + Default {}

#[derive(Debug)]
enum ClrWireError {
    Reader(String),
    Serialization(String),
}

#[derive(Debug)]
struct ClrReaderError(String);

impl fmt::Display for ClrReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug)]
enum ClrValidationError {
    Reader(String),
    Serialization(String),
    Context {
        field: String,
        source: Box<ClrValidationError>,
    },
}

fn clr_reader_error(message: &str) -> ClrValidationError {
    ClrValidationError::Reader(message.to_owned())
}

fn clr_serialization_error(message: impl Into<String>) -> ClrValidationError {
    ClrValidationError::Serialization(message.into())
}

impl fmt::Display for ClrValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reader(message) | Self::Serialization(message) => formatter.write_str(message),
            Self::Context { field, source } => write!(formatter, "{field}: {source}"),
        }
    }
}

impl ClrValidationError {
    fn is_reader_error(&self) -> bool {
        match self {
            Self::Reader(_) => true,
            Self::Serialization(_) => false,
            Self::Context { source, .. } => source.is_reader_error(),
        }
    }

    fn with_context(self, field: &str) -> Self {
        Self::Context {
            field: field.to_owned(),
            source: Box::new(self),
        }
    }
}

impl From<String> for ClrValidationError {
    fn from(message: String) -> Self {
        Self::Serialization(message)
    }
}

impl From<&str> for ClrValidationError {
    fn from(message: &str) -> Self {
        Self::Serialization(message.to_owned())
    }
}

fn clr_de_error<E: serde::de::Error>(error: ClrValidationError) -> E {
    if error.is_reader_error() {
        E::custom(ClrReaderError(error.to_string()))
    } else {
        E::custom(error.to_string())
    }
}

impl fmt::Display for ClrWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reader(message) | Self::Serialization(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ClrWireError {}

impl serde::de::Error for ClrWireError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        let message = message.to_string();
        if std::any::type_name::<T>() == std::any::type_name::<ClrReaderError>() {
            Self::Reader(message)
        } else {
            Self::Serialization(message)
        }
    }
}

struct ClrValueDeserializer<E> {
    value: Value,
    error: std::marker::PhantomData<E>,
}

impl<E> ClrValueDeserializer<E> {
    fn new(value: Value) -> Self {
        Self {
            value,
            error: std::marker::PhantomData,
        }
    }
}

impl<'de, E: serde::de::Error> serde::de::IntoDeserializer<'de, E> for ClrValueDeserializer<E> {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self::Deserializer {
        self
    }
}

impl<'de, E: serde::de::Error> Deserializer<'de> for ClrValueDeserializer<E> {
    type Error = E;

    fn deserialize_any<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        use serde::de::value::{
            BoolDeserializer, F64Deserializer, I64Deserializer, MapDeserializer, SeqDeserializer,
            StringDeserializer, U64Deserializer, UnitDeserializer,
        };

        match self.value {
            Value::Null => UnitDeserializer::<E>::new().deserialize_any(visitor),
            Value::Bool(value) => BoolDeserializer::<E>::new(value).deserialize_any(visitor),
            Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    I64Deserializer::<E>::new(value).deserialize_any(visitor)
                } else if let Some(value) = value.as_u64() {
                    U64Deserializer::<E>::new(value).deserialize_any(visitor)
                } else if let Some(value) = value.as_f64() {
                    F64Deserializer::<E>::new(value).deserialize_any(visitor)
                } else {
                    Err(E::custom("invalid JSON number"))
                }
            }
            Value::String(value) => StringDeserializer::<E>::new(value).deserialize_any(visitor),
            Value::Array(values) => {
                SeqDeserializer::<_, E>::new(values.into_iter().map(ClrValueDeserializer::<E>::new))
                    .deserialize_any(visitor)
            }
            Value::Object(values) => {
                MapDeserializer::<_, E>::new(values.into_iter().map(|(key, value)| {
                    (
                        StringDeserializer::<E>::new(key),
                        ClrValueDeserializer::<E>::new(value),
                    )
                }))
                .deserialize_any(visitor)
            }
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        if self.value.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn is_human_readable(&self) -> bool {
        true
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
        byte_buf unit unit_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

fn deserialize_clr_value<T: DeserializeOwned>(
    value: Value,
) -> std::result::Result<T, ClrWireError> {
    T::deserialize(ClrValueDeserializer::<ClrWireError>::new(value))
}

impl<'de, T> ClrCollection<'de> for Vec<T> where T: Deserialize<'de> {}

impl<'de, K, V> ClrCollection<'de> for BTreeMap<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
}

fn deserialize_clr_collection<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: ClrCollection<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_clr_string_list<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<Vec<Value>>::deserialize(deserializer)?;
    values
        .unwrap_or_default()
        .iter()
        .map(|value| clr_json_nullable_string(value).map_err(clr_de_error::<D::Error>))
        .collect()
}

fn deserialize_clr_nullable_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(Value::Number(value)) => Ok(Some(value.to_string())),
        Some(Value::Bool(value)) => Ok(Some(if value { "True" } else { "False" }.to_owned())),
        Some(_) => Err(clr_de_error::<D::Error>(clr_reader_error(
            "expected CLR string-compatible value",
        ))),
    }
}

fn deserialize_clr_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Bool(value) => Ok(value),
        Value::String(value) if value.trim().eq_ignore_ascii_case("true") => Ok(true),
        Value::String(value) if value.trim().eq_ignore_ascii_case("false") => Ok(false),
        Value::String(value) if value.is_empty() => Err(clr_de_error::<D::Error>(
            clr_serialization_error("Boolean cannot be null"),
        )),
        Value::String(_) => Err(clr_de_error::<D::Error>(clr_reader_error(
            "invalid Boolean string",
        ))),
        Value::Number(value) => value
            .as_i64()
            .map(|value| value != 0)
            .or_else(|| value.as_u64().map(|value| value != 0))
            .or_else(|| value.as_f64().map(|value| value != 0.0))
            .ok_or_else(|| clr_de_error::<D::Error>(clr_reader_error("invalid Boolean number"))),
        Value::Null => Err(clr_de_error::<D::Error>(clr_serialization_error(
            "Boolean cannot be null",
        ))),
        _ => Err(clr_de_error::<D::Error>(clr_reader_error(
            "expected CLR Boolean value",
        ))),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ClrInt32(i32);

impl<'de> Deserialize<'de> for ClrInt32 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Value::deserialize(deserializer)? {
            Value::Number(value) => clr_json_int32(&value)
                .map(Self)
                .map_err(clr_de_error::<D::Error>),
            Value::String(value) if value.is_empty() => Err(clr_de_error::<D::Error>(
                clr_serialization_error("Int32 cannot be null"),
            )),
            Value::String(value) => value
                .trim()
                .parse::<i32>()
                .map(Self)
                .map_err(|_| clr_de_error::<D::Error>(clr_reader_error("invalid Int32 string"))),
            Value::Null => Err(clr_de_error::<D::Error>(clr_serialization_error(
                "Int32 cannot be null",
            ))),
            _ => Err(clr_de_error::<D::Error>(clr_reader_error(
                "expected CLR Int32 value",
            ))),
        }
    }
}

fn clr_json_int32(value: &serde_json::Number) -> std::result::Result<i32, ClrValidationError> {
    if let Some(value) = value.as_i64() {
        return i32::try_from(value).map_err(|_| clr_reader_error("Int32 value is out of range"));
    }
    if let Some(value) = value.as_u64() {
        return i32::try_from(value).map_err(|_| clr_reader_error("Int32 value is out of range"));
    }
    value
        .as_f64()
        .map(f64::round_ties_even)
        .filter(|value| *value >= i32::MIN as f64 && *value <= i32::MAX as f64)
        .map(|value| value as i32)
        .ok_or_else(|| clr_reader_error("Int32 value is out of range"))
}

#[derive(Debug, Default)]
struct ClrTemplateToken;

impl<'de> Deserialize<'de> for ClrTemplateToken {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        validate_clr_template_token(&Value::deserialize(deserializer)?)
            .map_err(clr_de_error::<D::Error>)?;
        Ok(Self)
    }
}

#[derive(Debug, Default)]
struct ClrPipelineContextData;

impl<'de> Deserialize<'de> for ClrPipelineContextData {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        validate_clr_pipeline_context_data(&Value::deserialize(deserializer)?)
            .map_err(clr_de_error::<D::Error>)?;
        Ok(Self)
    }
}

fn clr_member<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
            .map(|(_, value)| value)
    })
}

fn validate_clr_nullable_string_value(
    value: &Value,
) -> std::result::Result<(), ClrValidationError> {
    match value {
        Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(()),
        _ => Err(clr_reader_error("expected CLR string-compatible value")),
    }
}

fn validate_clr_int32_value(
    value: &Value,
    nullable: bool,
) -> std::result::Result<(), ClrValidationError> {
    match value {
        Value::Null if nullable => Ok(()),
        Value::Null => Err(clr_serialization_error("Int32 cannot be null")),
        Value::Number(number) => clr_json_int32(number).map(|_| ()),
        Value::String(value) if value.is_empty() && nullable => Ok(()),
        Value::String(value) if value.is_empty() => {
            Err(clr_serialization_error("Int32 cannot be null"))
        }
        Value::String(value) if value.trim().parse::<i32>().is_ok() => Ok(()),
        Value::String(_) | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            Err(clr_reader_error("invalid Int32 value"))
        }
    }
}

fn validate_clr_bool_value(
    value: &Value,
    nullable: bool,
) -> std::result::Result<(), ClrValidationError> {
    match value {
        Value::Null if nullable => Ok(()),
        Value::Null => Err(clr_serialization_error("Boolean cannot be null")),
        Value::Bool(_) => Ok(()),
        Value::String(value)
            if value.trim().eq_ignore_ascii_case("true")
                || value.trim().eq_ignore_ascii_case("false") =>
        {
            Ok(())
        }
        Value::String(value) if value.is_empty() && nullable => Ok(()),
        Value::String(value) if value.is_empty() => {
            Err(clr_serialization_error("Boolean cannot be null"))
        }
        Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Err(clr_reader_error("invalid Boolean value"))
        }
        Value::Number(_) => Ok(()),
    }
}

fn validate_clr_double_value(value: &Value) -> std::result::Result<(), ClrValidationError> {
    match value {
        Value::Number(_) => Ok(()),
        Value::Bool(_) => Err(clr_reader_error("invalid Double value")),
        Value::String(value) if value.is_empty() => {
            Err(clr_serialization_error("Double cannot be null"))
        }
        Value::String(value) if parse_clr_double_text(value).is_some() => Ok(()),
        Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Err(clr_reader_error("invalid Double value"))
        }
        Value::Null => Err(clr_serialization_error("Double cannot be null")),
    }
}

fn clr_converter_integer_discriminator(
    value: &serde_json::Number,
    field: &str,
) -> std::result::Result<Option<i32>, ClrValidationError> {
    if value.is_f64() {
        // The pinned converters dispatch only integer JTokens. Float tokens
        // return their existing null value without reading an Int32.
        return Ok(None);
    }
    if let Some(value) = value.as_i64() {
        return i32::try_from(value)
            .map(Some)
            .map_err(|_| clr_serialization_error(format!("{field} integer is outside Int32")));
    }
    if let Some(value) = value.as_u64() {
        return i32::try_from(value)
            .map(Some)
            .map_err(|_| clr_serialization_error(format!("{field} integer is outside Int32")));
    }
    Err(clr_serialization_error(format!(
        "{field} integer is outside Int32"
    )))
}

fn validate_clr_template_token(value: &Value) -> std::result::Result<(), ClrValidationError> {
    let Value::Object(object) = value else {
        // TemplateTokenJsonConverter casts integer reader values to Int64
        // before constructing NumberToken. Overflow is a retryable conversion
        // exception, while ordinary scalar/array/null forms are accepted.
        validate_clr_dynamic_integer(value)?;
        return Ok(());
    };

    let token_type = match clr_member(object, "type") {
        None => 0, // Missing discriminator defaults to String.
        Some(Value::Number(value)) => {
            let Some(value) = clr_converter_integer_discriminator(value, "TemplateToken type")?
            else {
                return Ok(());
            };
            value
        }
        Some(_) => return Ok(()), // The converter returns its existing null value.
    };

    for member in ["file", "line", "col"] {
        if let Some(value) = clr_member(object, member) {
            validate_clr_int32_value(value, true).map_err(|error| error.with_context(member))?;
        }
    }

    match token_type {
        0 => {
            if let Some(value) = clr_member(object, "lit") {
                validate_clr_nullable_string_value(value)
                    .map_err(|error| error.with_context("lit"))?;
            }
        }
        1 => {
            if let Some(value) = clr_member(object, "seq") {
                match value {
                    Value::Null => {}
                    Value::Array(items) => {
                        for item in items {
                            validate_clr_template_token(item)?;
                        }
                    }
                    _ => return Err("TemplateToken sequence must be an array".into()),
                }
            }
        }
        2 => {
            if let Some(value) = clr_member(object, "map") {
                match value {
                    Value::Null => {}
                    Value::Array(items) => {
                        for pair in items {
                            let Some(pair) = pair.as_object() else {
                                return Err("TemplateToken map item must be an object".into());
                            };
                            if let Some(key) = clr_member(pair, "key") {
                                validate_clr_template_token(key)?;
                                if let Value::Object(key) = key
                                    && let Some(Value::Number(kind)) = clr_member(key, "type")
                                    && let Some(kind) = clr_converter_integer_discriminator(
                                        kind,
                                        "TemplateToken type",
                                    )?
                                {
                                    // BasicExpressionToken and InsertExpressionToken
                                    // derive from ScalarToken and are valid map keys.
                                    if !matches!(kind, 0 | 3 | 4 | 5 | 6 | 7) {
                                        return Err("TemplateToken map key must be scalar".into());
                                    }
                                }
                            }
                            if let Some(value) = clr_member(pair, "value") {
                                validate_clr_template_token(value)?;
                            }
                        }
                    }
                    _ => return Err("TemplateToken map must be an array".into()),
                }
            }
        }
        3 => {
            if let Some(value) = clr_member(object, "expr") {
                validate_clr_nullable_string_value(value)
                    .map_err(|error| error.with_context("expr"))?;
            }
        }
        4 | 7 => {}
        5 => {
            if let Some(value) = clr_member(object, "bool") {
                validate_clr_bool_value(value, false)
                    .map_err(|error| error.with_context("bool"))?;
            }
        }
        6 => {
            if let Some(value) = clr_member(object, "num") {
                validate_clr_double_value(value).map_err(|error| error.with_context("num"))?;
            }
        }
        _ => return Err("unknown TemplateToken type".into()),
    }
    Ok(())
}

fn validate_clr_pipeline_context_data(
    value: &Value,
) -> std::result::Result<(), ClrValidationError> {
    let Value::Object(object) = value else {
        // PipelineContextDataJsonConverter casts integer reader values to
        // Int64 before constructing NumberContextData.
        validate_clr_dynamic_integer(value)?;
        return Ok(());
    };

    let context_type = match clr_member(object, "t") {
        None => 0,
        Some(Value::Number(value)) => {
            let Some(value) =
                clr_converter_integer_discriminator(value, "PipelineContextData type")?
            else {
                return Ok(());
            };
            value
        }
        Some(_) => return Ok(()),
    };

    match context_type {
        0 => {
            if let Some(value) = clr_member(object, "s") {
                validate_clr_nullable_string_value(value)
                    .map_err(|error| error.with_context("s"))?;
            }
        }
        1 => {
            if let Some(value) = clr_member(object, "a") {
                match value {
                    Value::Null => {}
                    Value::Array(items) => {
                        for item in items {
                            validate_clr_pipeline_context_data(item)?;
                        }
                    }
                    _ => return Err("PipelineContextData array must be an array".into()),
                }
            }
        }
        2 | 5 => {
            if let Some(value) = clr_member(object, "d") {
                match value {
                    Value::Null => {}
                    Value::Array(items) => {
                        for item in items {
                            if item.is_null() {
                                // The pinned pair type is a reference class,
                                // so Json.NET permits null list entries here.
                                continue;
                            }
                            let Some(pair) = item.as_object() else {
                                return Err(
                                    "PipelineContextData dictionary item must be an object".into(),
                                );
                            };
                            if let Some(key) = clr_member(pair, "k") {
                                validate_clr_nullable_string_value(key)
                                    .map_err(|error| error.with_context("k"))?;
                            }
                            if let Some(value) = clr_member(pair, "v") {
                                validate_clr_pipeline_context_data(value)?;
                            }
                        }
                    }
                    _ => return Err("PipelineContextData dictionary must be an array".into()),
                }
            }
        }
        3 => {
            if let Some(value) = clr_member(object, "b") {
                validate_clr_bool_value(value, false).map_err(|error| error.with_context("b"))?;
            }
        }
        4 => {
            if let Some(value) = clr_member(object, "n") {
                validate_clr_double_value(value).map_err(|error| error.with_context("n"))?;
            }
        }
        _ => return Err("unknown PipelineContextData type".into()),
    }
    Ok(())
}

fn validate_clr_dynamic_integer(value: &Value) -> std::result::Result<(), ClrValidationError> {
    let Value::Number(number) = value else {
        return Ok(());
    };
    if number.is_f64() {
        return Ok(());
    }
    if number.as_i64().is_some()
        || number
            .as_u64()
            .is_some_and(|integer| integer <= i64::MAX as u64)
    {
        Ok(())
    } else {
        Err(clr_serialization_error(
            "dynamic integer token is outside Int64",
        ))
    }
}

/// Keep the protocol's CLR-oriented name at call sites while sharing the
/// ordered token tree and source-aware scanner with the wire DTO parser.
type ClrOrderedValue = OrderedJsonValue;

fn collapse_exact_clr_context_entries(
    entries: Vec<(String, ClrOrderedValue)>,
) -> Vec<(String, ContextValue)> {
    let mut collapsed = Vec::<(String, ContextValue)>::with_capacity(entries.len());
    for (key, value) in entries {
        let value = value.into_context_value();
        if let Some((_, existing)) = collapsed.iter_mut().find(|(existing, _)| existing == &key) {
            // JObject replaces an exact duplicate in place, preserving the
            // original property position while assigning the later value.
            *existing = value;
        } else {
            collapsed.push((key, value));
        }
    }
    collapsed
}

fn context_value_wire_value(
    entries: Vec<(String, ClrOrderedValue)>,
    case_sensitive: bool,
) -> Value {
    let entries = collapse_exact_clr_context_entries(entries);
    let value = ContextValue::Object {
        case_sensitive,
        entries,
    };
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn parse_clr_ordered_json(body: &str) -> std::result::Result<ClrOrderedValue, serde_json::Error> {
    parse_json_text(body)
}

fn approximate_clr_big_integer(value: &str) -> Value {
    clr_big_integer_to_f64(value)
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(value.to_owned()))
}

fn clr_big_integer_double_value(decimal: &str) -> Value {
    let Some(value) = clr_big_integer_to_f64(decimal) else {
        return Value::String(decimal.to_owned());
    };
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or_else(|| {
            Value::String(
                non_finite_text(if value.is_sign_negative() {
                    NonFinite::NegativeInfinity
                } else {
                    NonFinite::PositiveInfinity
                })
                .to_owned(),
            )
        })
}

fn ordered_number_value(number: JsonNumber) -> Value {
    match number.kind {
        JsonNumberKind::Int64(value) => Value::Number(value.into()),
        JsonNumberKind::Float(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsonNumberKind::BigInteger(value) => approximate_clr_big_integer(&value),
    }
}

fn ordered_number_text(number: JsonNumber) -> String {
    if number.origin == JsonReaderOrigin::TextReader {
        return number.lexeme;
    }
    match number.kind {
        JsonNumberKind::Int64(value) => value.to_string(),
        JsonNumberKind::Float(value) => dotnet_double_general(value),
        JsonNumberKind::BigInteger(value) => value,
    }
}

impl OrderedJsonValue {
    fn collapse_exact_properties(self) -> Self {
        match self {
            Self::Array(values) => Self::Array(
                values
                    .into_iter()
                    .map(Self::collapse_exact_properties)
                    .collect(),
            ),
            Self::Object(entries) => {
                let mut collapsed: Vec<(String, ClrOrderedValue)> = Vec::new();
                for (name, value) in entries {
                    let value = value.collapse_exact_properties();
                    if let Some((_, previous)) =
                        collapsed.iter_mut().find(|(existing, _)| existing == &name)
                    {
                        *previous = value;
                    } else {
                        collapsed.push((name, value));
                    }
                }
                Self::Object(collapsed)
            }
            Self::Constructor { name, arguments } => Self::Constructor {
                name,
                arguments: arguments
                    .into_iter()
                    .map(Self::collapse_exact_properties)
                    .collect(),
            },
            value => value,
        }
    }

    fn into_context_value(self) -> ContextValue {
        match self {
            Self::Null => ContextValue::Null,
            Self::Bool(value) => ContextValue::Bool(value),
            Self::Number(JsonNumber {
                kind: JsonNumberKind::Int64(value),
                ..
            }) => ContextValue::Number(value.into()),
            Self::Number(JsonNumber {
                kind: JsonNumberKind::Float(value),
                ..
            }) => match serde_json::Number::from_f64(value) {
                Some(number) => ContextValue::Number(number),
                None => ContextValue::NonFinite(if value.is_nan() {
                    NonFinite::NaN
                } else if value.is_sign_negative() {
                    NonFinite::NegativeInfinity
                } else {
                    NonFinite::PositiveInfinity
                }),
            },
            Self::Number(JsonNumber {
                kind: JsonNumberKind::BigInteger(value),
                ..
            }) => ContextValue::BigInteger(value),
            Self::NonFinite { value, .. } => ContextValue::non_finite(value),
            Self::String(value) => ContextValue::String(value),
            Self::Undefined => ContextValue::Undefined,
            Self::Constructor { name, arguments } => ContextValue::Constructor {
                name,
                arguments: arguments
                    .into_iter()
                    .map(Self::into_context_value)
                    .collect(),
            },
            Self::Array(values) => {
                ContextValue::Array(values.into_iter().map(Self::into_context_value).collect())
            }
            Self::Object(values) => ContextValue::Object {
                case_sensitive: true,
                entries: collapse_exact_clr_context_entries(values),
            },
        }
    }

    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Number(number) => ordered_number_value(number),
            // serde_json cannot represent CLR's bare NaN/Infinity literals.
            // Keep them out of raw/JToken projections; typed Double slots use
            // the explicit bridge in `into_clr_value` below. `raw_json` keeps
            // the exact response for quarantine and diagnostics.
            Self::NonFinite { .. } => Value::Null,
            Self::Undefined | Self::Constructor { .. } => Value::Null,
            Self::String(value) => Value::String(value),
            Self::Array(values) => Value::Array(values.into_iter().map(Self::into_value).collect()),
            Self::Object(values) => {
                let mut object = serde_json::Map::new();
                for (key, value) in values {
                    object.insert(key, value.into_value());
                }
                Value::Object(object)
            }
        }
    }

    fn into_clr_value(self, shape: ClrWireShape) -> Value {
        match shape {
            ClrWireShape::Raw => self.into_value(),
            ClrWireShape::String => self.into_clr_string_value(),
            ClrWireShape::Guid
            | ClrWireShape::Int32
            | ClrWireShape::Int64
            | ClrWireShape::Boolean
            | ClrWireShape::DateTime => self.into_value(),
            ClrWireShape::Uri => match self {
                Self::Null | Self::Undefined => Value::Null,
                Self::String(value) if value.is_empty() => Value::Null,
                value => value.into_value(),
            },
            ClrWireShape::Double => match self {
                Self::NonFinite { value, .. } => Value::String(non_finite_text(value).to_owned()),
                Self::Number(JsonNumber {
                    kind: JsonNumberKind::BigInteger(value),
                    ..
                }) => clr_big_integer_double_value(&value),
                value => value.into_value(),
            },
            ClrWireShape::Object(schema) => self.into_clr_object(schema),
            ClrWireShape::Array(schema) => match self {
                Self::Array(values) => Value::Array(
                    values
                        .into_iter()
                        .map(|value| value.into_clr_value(ClrWireShape::Object(schema)))
                        .collect(),
                ),
                value => value.into_value(),
            },
            ClrWireShape::ArrayTemplateToken => match self {
                Self::Array(values) => Value::Array(
                    values
                        .into_iter()
                        .map(|value| value.into_clr_value(ClrWireShape::TemplateToken))
                        .collect(),
                ),
                value => value.into_value(),
            },
            ClrWireShape::ArrayPipelineContextData => match self {
                Self::Array(values) => Value::Array(
                    values
                        .into_iter()
                        .map(|value| value.into_clr_value(ClrWireShape::PipelineContextData))
                        .collect(),
                ),
                value => value.into_value(),
            },
            ClrWireShape::RawArray => match self {
                Self::Array(values) => Value::Array(
                    values
                        .into_iter()
                        .map(Self::into_clr_string_value)
                        .collect(),
                ),
                value => value.into_value(),
            },
            ClrWireShape::MapValues(schema) => match self {
                Self::Object(values) => {
                    if matches!(schema, ClrWireSchema::PipelineContextData) {
                        let mut entries = Vec::with_capacity(values.len());
                        for (key, value) in values {
                            let value = value.into_clr_value(ClrWireShape::PipelineContextData);
                            if let Some((_, previous)) =
                                entries.iter_mut().find(|(name, _)| name == &key)
                            {
                                *previous = value;
                            } else {
                                entries.push((key, value));
                            }
                        }
                        crate::job_message::ordered_context_data_pair_array_value(entries)
                    } else {
                        let mut object = serde_json::Map::new();
                        for (key, value) in values {
                            let value = value.into_clr_value(ClrWireShape::Object(schema));
                            object.insert(key, value);
                        }
                        Value::Object(object)
                    }
                }
                Self::Null | Self::Undefined => Value::Null,
                Self::Array(_) => Value::Bool(false),
                value => value.into_value(),
            },
            ClrWireShape::CaseInsensitiveStringMap => match self {
                Self::Object(values) => {
                    let mut object = serde_json::Map::new();
                    for (key, value) in values {
                        insert_case_insensitive(&mut object, key, value.into_clr_string_value());
                    }
                    Value::Object(object)
                }
                value => value.into_value(),
            },
            ClrWireShape::ExactStringMap => match self {
                Self::Object(values) => {
                    let mut object = serde_json::Map::new();
                    for (key, value) in values {
                        object.insert(key, value.into_clr_string_value());
                    }
                    Value::Object(object)
                }
                value => value.into_value(),
            },
            ClrWireShape::PropertyBag => match self {
                Self::Object(values) => context_value_wire_value(values, false),
                Self::Null => Value::Null,
                value => value.into_value(),
            },
            ClrWireShape::JTokenObject => match self {
                Self::Object(values) => context_value_wire_value(values, true),
                Self::Null => Value::Null,
                // Option<JObject> accepts null but rejects an Undefined
                // JValue or JConstructor at this typed root boundary.
                Self::Undefined | Self::Constructor { .. } => Value::Bool(false),
                value => value.into_value(),
            },
            ClrWireShape::ExactNullableStringMap => match self {
                Self::Object(values) => {
                    let mut object = serde_json::Map::new();
                    for (key, value) in values {
                        object.insert(key, value.into_clr_string_value());
                    }
                    Value::Object(object)
                }
                value => value.into_value(),
            },
            ClrWireShape::Links => self.into_clr_links(),
            ClrWireShape::TemplateToken => match self {
                Self::NonFinite { value, .. } => {
                    json!({"Type": 6, "Num": non_finite_text(value)})
                }
                value => value.into_clr_template_token(),
            },
            ClrWireShape::PipelineContextData => match self {
                Self::NonFinite { value, .. } => {
                    json!({"T": 4, "N": non_finite_text(value)})
                }
                value => value.into_clr_context_data(),
            },
            ClrWireShape::Steps => self.into_clr_steps(),
            ClrWireShape::ActionReference => self.into_clr_action_reference(),
        }
    }

    fn into_clr_object(self, schema: ClrWireSchema) -> Value {
        let Self::Object(values) = self else {
            return self.into_value();
        };
        let mut object = serde_json::Map::new();
        for (key, value) in values {
            let Some(field) = clr_wire_fields(schema)
                .iter()
                .find(|field| clr_ordinal_ignore_case_eq(&key, field.name))
            else {
                // Json.NET ignores unknown CLR object properties. Preserve
                // their value without interpreting dictionaries or JTokens.
                object.insert(key, value.into_value());
                continue;
            };
            let previous = object.remove(field.name);
            let normalized = match field.shape {
                ClrWireShape::TemplateToken => {
                    if matches!(&value, ClrOrderedValue::NonFinite { .. }) {
                        value.into_clr_value(ClrWireShape::TemplateToken)
                    } else {
                        value.into_clr_template_token_with_existing(previous.clone())
                    }
                }
                ClrWireShape::PipelineContextData => {
                    if matches!(&value, ClrOrderedValue::NonFinite { .. }) {
                        value.into_clr_value(ClrWireShape::PipelineContextData)
                    } else {
                        value.into_clr_context_data_with_existing(previous.clone())
                    }
                }
                shape => value.into_clr_value(shape),
            };
            if let Some(previous) = previous {
                let normalized = match field.shape {
                    ClrWireShape::TemplateToken | ClrWireShape::PipelineContextData => normalized,
                    shape => merge_clr_wire_value(shape, previous, normalized),
                };
                object.insert(field.name.to_owned(), normalized);
            } else {
                object.insert(field.name.to_owned(), normalized);
            }
        }
        Value::Object(object)
    }

    /// Json.NET's `ReadAsString` converts primitive values at typed string
    /// dictionary boundaries. Opaque JToken values continue through
    /// `into_value`, where nonfinite values are intentionally not stringified.
    fn into_clr_string_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::String(value) => Value::String(value),
            Self::Bool(value) => Value::String(if value { "True" } else { "False" }.to_owned()),
            Self::Number(value) => Value::String(ordered_number_text(value)),
            Self::NonFinite {
                value,
                origin,
                lexeme,
            } => Value::String(match origin {
                JsonReaderOrigin::TextReader => lexeme,
                JsonReaderOrigin::JObjectReader => non_finite_text(value).to_owned(),
            }),
            value => value.into_value(),
        }
    }

    fn into_clr_links(self) -> Value {
        let Self::Object(values) = self else {
            return self.into_value();
        };
        let mut object = serde_json::Map::new();
        for (key, value) in values {
            let value = match value {
                Self::Array(items) => Value::Array(
                    items
                        .into_iter()
                        .map(|item| {
                            item.into_clr_value(ClrWireShape::Object(ClrWireSchema::ReferenceLink))
                        })
                        .collect(),
                ),
                item => item.into_clr_value(ClrWireShape::Object(ClrWireSchema::ReferenceLink)),
            };
            object.insert(key, value);
        }
        Value::Object(object)
    }

    fn into_clr_template_token(self) -> Value {
        self.into_clr_template_token_with_existing(None)
    }

    fn into_clr_template_token_with_existing(self, existing: Option<Value>) -> Value {
        let mut value = self.collapse_exact_properties();
        if matches!(&value, Self::NonFinite { .. }) {
            return value.into_clr_value(ClrWireShape::TemplateToken);
        }
        if matches!(&value, Self::Object(_)) {
            value.set_number_origin(JsonReaderOrigin::JObjectReader);
        }
        let Self::Object(values) = value else {
            return value.into_value();
        };
        let kind = match ordered_member(&values, "type") {
            Some(value) => match ordered_converter_i32(value, "TemplateToken type") {
                Ok(Some(kind)) => kind,
                Ok(None) => return existing.unwrap_or(Value::Null),
                Err(_) => return Value::Null,
            },
            None => 0,
        };
        let fields = clr_template_token_fields(kind);
        Self::normalize_named_fields(values, &fields, Some(("Type", kind)))
    }

    fn into_clr_context_data(self) -> Value {
        self.into_clr_context_data_with_existing(None)
    }

    fn into_clr_context_data_with_existing(self, existing: Option<Value>) -> Value {
        let mut value = self.collapse_exact_properties();
        if matches!(&value, Self::NonFinite { .. }) {
            return value.into_clr_value(ClrWireShape::PipelineContextData);
        }
        if matches!(&value, Self::Object(_)) {
            value.set_number_origin(JsonReaderOrigin::JObjectReader);
        }
        let Self::Object(values) = value else {
            return match value {
                Self::Null | Self::Array(_) => Value::Null,
                Self::Number(JsonNumber {
                    kind: JsonNumberKind::Int64(value),
                    ..
                }) => serde_json::Number::from_f64(value as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                Self::Number(JsonNumber {
                    kind: JsonNumberKind::Float(value),
                    ..
                }) => serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .unwrap_or_else(|| {
                        Value::String(
                            non_finite_text(if value.is_nan() {
                                NonFinite::NaN
                            } else if value.is_sign_negative() {
                                NonFinite::NegativeInfinity
                            } else {
                                NonFinite::PositiveInfinity
                            })
                            .to_owned(),
                        )
                    }),
                // `validate_clr_context_occurrences` rejects typed BigInteger
                // scalars before this projection, matching the converter's
                // checked `(Int64)` cast.
                value => value.into_value(),
            };
        };
        let kind = match ordered_member(&values, "t") {
            Some(value) => match ordered_converter_i32(value, "PipelineContextData type") {
                Ok(Some(kind)) => kind,
                Ok(None) => return existing.unwrap_or(Value::Null),
                Err(_) => return Value::Null,
            },
            None => 0,
        };
        let fields = clr_context_data_fields(kind);
        Self::normalize_named_fields(values, &fields, Some(("T", kind)))
    }

    fn into_clr_steps(self) -> Value {
        let Self::Array(steps) = self else {
            return self.into_value();
        };
        Value::Array(
            steps
                .into_iter()
                .map(|step| {
                    let step = step.collapse_exact_properties();
                    let Self::Object(fields) = &step else {
                        return step.into_value();
                    };
                    let kind = ordered_member(fields, "Type").and_then(ordered_step_type);
                    match kind {
                        Some(4) => {
                            step.into_clr_converter_object(ClrWireSchema::ActionStep, "Type", 4)
                        }
                        Some(5) => {
                            step.into_clr_converter_object(ClrWireSchema::BackgroundStep, "Type", 5)
                        }
                        _ => Value::Null,
                    }
                })
                .collect(),
        )
    }

    fn into_clr_action_reference(self) -> Value {
        let mut value = self.collapse_exact_properties();
        if matches!(&value, Self::Object(_)) {
            value.set_number_origin(JsonReaderOrigin::JObjectReader);
        }
        let Self::Object(values) = value else {
            return value.into_value();
        };
        let kind = ordered_member(&values, "Type").and_then(ordered_action_type);
        let Some(kind) = kind else {
            return Value::Null;
        };
        let fields = clr_action_reference_fields(kind);
        Self::normalize_named_fields(values, &fields, Some(("Type", kind)))
    }

    fn normalize_named_fields(
        values: Vec<(String, Self)>,
        fields: &[ClrWireField],
        discriminator: Option<(&str, i32)>,
    ) -> Value {
        let mut object = serde_json::Map::new();
        for (key, value) in values {
            let Some(field) = fields
                .iter()
                .find(|field| clr_ordinal_ignore_case_eq(&key, field.name))
            else {
                continue;
            };
            let previous = object.remove(field.name);
            let normalized = value.into_clr_value(field.shape);
            let normalized = match (is_mutable_converter_collection(field), previous) {
                (true, Some(previous)) => merge_clr_wire_value(field.shape, previous, normalized),
                _ => normalized,
            };
            object.insert(field.name.to_owned(), normalized);
        }
        if let Some((name, kind)) = discriminator {
            object.insert(name.to_owned(), Value::from(kind));
        }
        Value::Object(object)
    }

    fn into_clr_converter_object(
        self,
        schema: ClrWireSchema,
        discriminator: &'static str,
        kind: i32,
    ) -> Value {
        let Self::Object(values) = self else {
            return self.into_value();
        };
        let mut object = serde_json::Map::new();
        for (key, value) in values {
            let Some(field) = clr_wire_fields(schema)
                .iter()
                .find(|field| clr_ordinal_ignore_case_eq(&key, field.name))
            else {
                continue;
            };
            let previous = object.remove(field.name);
            let normalized = match field.shape {
                ClrWireShape::TemplateToken => {
                    if matches!(&value, ClrOrderedValue::NonFinite { .. }) {
                        value.into_clr_value(ClrWireShape::TemplateToken)
                    } else {
                        value.into_clr_template_token_with_existing(previous.clone())
                    }
                }
                ClrWireShape::PipelineContextData => {
                    if matches!(&value, ClrOrderedValue::NonFinite { .. }) {
                        value.into_clr_value(ClrWireShape::PipelineContextData)
                    } else {
                        value.into_clr_context_data_with_existing(previous.clone())
                    }
                }
                shape => value.into_clr_value(shape),
            };
            // Json.NET Populate reuses mutable collection properties. Aliases
            // append into the constructor's backing list; null assigns null,
            // and a later array then starts a fresh list.
            let normalized = match (is_mutable_converter_collection(field), previous) {
                (true, Some(previous)) => merge_clr_wire_value(field.shape, previous, normalized),
                _ => normalized,
            };
            object.insert(field.name.to_owned(), normalized);
        }
        object.insert(discriminator.to_owned(), Value::from(kind));
        Value::Object(object)
    }
}

#[derive(Clone, Copy)]
enum ClrWireSchema {
    AgentJob,
    Plan,
    Owner,
    Timeline,
    JobResources,
    RepositoryResource,
    ContainerResource,
    ServiceEndpoint,
    ServiceEndpointReference,
    EndpointAuthorization,
    VariableValue,
    MaskHint,
    Workspace,
    ActionsEnvironment,
    DebuggerTunnel,
    ReferenceLink,
    TemplatePair,
    PipelineContextData,
    ContextPair,
    ActionStep,
    BackgroundStep,
}

#[derive(Clone, Copy)]
enum ClrWireShape {
    Raw,
    String,
    Guid,
    Int32,
    Int64,
    Boolean,
    DateTime,
    Uri,
    Double,
    Object(ClrWireSchema),
    Array(ClrWireSchema),
    MapValues(ClrWireSchema),
    CaseInsensitiveStringMap,
    ExactStringMap,
    ExactNullableStringMap,
    PropertyBag,
    JTokenObject,
    Links,
    TemplateToken,
    PipelineContextData,
    Steps,
    ActionReference,
    ArrayTemplateToken,
    ArrayPipelineContextData,
    RawArray,
}

fn is_mutable_converter_collection(field: &ClrWireField) -> bool {
    matches!(
        (field.name, field.shape),
        ("Seq", ClrWireShape::ArrayTemplateToken)
            | ("Map", ClrWireShape::Array(ClrWireSchema::TemplatePair))
            | ("A", ClrWireShape::ArrayPipelineContextData)
            | ("D", ClrWireShape::Array(ClrWireSchema::ContextPair))
    )
}

#[derive(Clone, Copy)]
struct ClrWireField {
    name: &'static str,
    shape: ClrWireShape,
}

impl ClrWireField {
    const fn raw(name: &'static str) -> Self {
        Self {
            name,
            shape: ClrWireShape::Raw,
        }
    }

    const fn typed(name: &'static str, shape: ClrWireShape) -> Self {
        Self { name, shape }
    }

    const fn string(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::String)
    }

    const fn guid(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::Guid)
    }

    const fn int32(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::Int32)
    }

    const fn int64(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::Int64)
    }

    const fn boolean(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::Boolean)
    }

    const fn date_time(name: &'static str) -> Self {
        Self::typed(name, ClrWireShape::DateTime)
    }
}

fn clr_template_token_fields(kind: i32) -> Vec<ClrWireField> {
    let mut fields = vec![
        ClrWireField::raw("Type"),
        ClrWireField::int32("File"),
        ClrWireField::int32("Line"),
        ClrWireField::int32("Col"),
    ];
    match kind {
        0 => fields.push(ClrWireField::string("Lit")),
        1 => fields.push(ClrWireField::typed("Seq", ClrWireShape::ArrayTemplateToken)),
        2 => fields.push(ClrWireField::typed(
            "Map",
            ClrWireShape::Array(ClrWireSchema::TemplatePair),
        )),
        3 | 4 => fields.push(ClrWireField::string("Expr")),
        5 => fields.push(ClrWireField::boolean("Bool")),
        6 => fields.push(ClrWireField::typed("Num", ClrWireShape::Double)),
        _ => {}
    }
    fields
}

fn clr_context_data_fields(kind: i32) -> Vec<ClrWireField> {
    let mut fields = vec![ClrWireField::raw("T")];
    match kind {
        0 => fields.push(ClrWireField::string("S")),
        1 => fields.push(ClrWireField::typed(
            "A",
            ClrWireShape::ArrayPipelineContextData,
        )),
        2 | 5 => fields.push(ClrWireField::typed(
            "D",
            ClrWireShape::Array(ClrWireSchema::ContextPair),
        )),
        3 => fields.push(ClrWireField::boolean("B")),
        4 => fields.push(ClrWireField::typed("N", ClrWireShape::Double)),
        _ => {}
    }
    fields
}

fn clr_action_reference_fields(kind: i32) -> Vec<ClrWireField> {
    let mut fields = vec![ClrWireField::raw("Type")];
    match kind {
        1 => {
            for name in ["Name", "Ref", "RepositoryType", "Path"] {
                fields.push(ClrWireField::string(name));
            }
        }
        2 => fields.push(ClrWireField::string("Image")),
        _ => {}
    }
    fields
}

const ROOT_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("MessageType"),
    ClrWireField::typed("Plan", ClrWireShape::Object(ClrWireSchema::Plan)),
    ClrWireField::typed("Timeline", ClrWireShape::Object(ClrWireSchema::Timeline)),
    ClrWireField::guid("JobId"),
    ClrWireField::string("JobDisplayName"),
    ClrWireField::string("JobName"),
    ClrWireField::typed("JobContainer", ClrWireShape::TemplateToken),
    ClrWireField::typed("JobServiceContainers", ClrWireShape::TemplateToken),
    ClrWireField::typed("JobOutputs", ClrWireShape::TemplateToken),
    ClrWireField::int64("RequestId"),
    ClrWireField::date_time("LockedUntil"),
    ClrWireField::typed(
        "Resources",
        ClrWireShape::Object(ClrWireSchema::JobResources),
    ),
    ClrWireField::typed(
        "ContextData",
        ClrWireShape::MapValues(ClrWireSchema::PipelineContextData),
    ),
    ClrWireField::typed("Workspace", ClrWireShape::Object(ClrWireSchema::Workspace)),
    ClrWireField::typed("EnvironmentVariables", ClrWireShape::ArrayTemplateToken),
    ClrWireField::typed(
        "Variables",
        ClrWireShape::MapValues(ClrWireSchema::VariableValue),
    ),
    ClrWireField::typed("Mask", ClrWireShape::Array(ClrWireSchema::MaskHint)),
    ClrWireField::typed("Steps", ClrWireShape::Steps),
    ClrWireField::typed("Defaults", ClrWireShape::ArrayTemplateToken),
    ClrWireField::typed(
        "ActionsEnvironment",
        ClrWireShape::Object(ClrWireSchema::ActionsEnvironment),
    ),
    ClrWireField::typed("Snapshot", ClrWireShape::TemplateToken),
    ClrWireField::string("BillingOwnerId"),
    ClrWireField::boolean("EnableDebugger"),
    ClrWireField::typed(
        "DebuggerTunnel",
        ClrWireShape::Object(ClrWireSchema::DebuggerTunnel),
    ),
    ClrWireField::string("DebuggerWelcomeMessage"),
    ClrWireField::typed("dependencies", ClrWireShape::RawArray),
    ClrWireField::typed("FileTable", ClrWireShape::RawArray),
    ClrWireField::typed("JobSidecarContainers", ClrWireShape::ExactNullableStringMap),
];

const PLAN_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::guid("ScopeIdentifier"),
    ClrWireField::string("PlanType"),
    ClrWireField::int32("Version"),
    ClrWireField::guid("PlanId"),
    ClrWireField::string("PlanGroup"),
    ClrWireField::typed("ArtifactUri", ClrWireShape::Uri),
    ClrWireField::typed("ArtifactLocation", ClrWireShape::Uri),
    ClrWireField::typed("Definition", ClrWireShape::Object(ClrWireSchema::Owner)),
    ClrWireField::typed("Owner", ClrWireShape::Object(ClrWireSchema::Owner)),
];
const OWNER_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::int32("Id"),
    ClrWireField::string("Name"),
    ClrWireField::typed("_links", ClrWireShape::Links),
];
const TIMELINE_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::guid("Id"),
    ClrWireField::int32("ChangeId"),
    ClrWireField::typed("Location", ClrWireShape::Uri),
];
const JOB_RESOURCES_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::typed(
        "Endpoints",
        ClrWireShape::Array(ClrWireSchema::ServiceEndpoint),
    ),
    ClrWireField::typed(
        "Repositories",
        ClrWireShape::Array(ClrWireSchema::RepositoryResource),
    ),
    ClrWireField::typed(
        "Containers",
        ClrWireShape::Array(ClrWireSchema::ContainerResource),
    ),
];
const RESOURCE_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("Alias"),
    ClrWireField::typed(
        "Endpoint",
        ClrWireShape::Object(ClrWireSchema::ServiceEndpointReference),
    ),
    ClrWireField::typed("Properties", ClrWireShape::PropertyBag),
];
const SERVICE_ENDPOINT_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::guid("Id"),
    ClrWireField::string("Name"),
    ClrWireField::string("Type"),
    ClrWireField::string("Owner"),
    ClrWireField::typed("Url", ClrWireShape::Uri),
    ClrWireField::string("Description"),
    ClrWireField::typed(
        "Authorization",
        ClrWireShape::Object(ClrWireSchema::EndpointAuthorization),
    ),
    ClrWireField::guid("GroupScopeId"),
    ClrWireField::typed("Data", ClrWireShape::CaseInsensitiveStringMap),
    ClrWireField::boolean("IsShared"),
    ClrWireField::boolean("IsReady"),
    ClrWireField::typed("OperationStatus", ClrWireShape::JTokenObject),
];
const SERVICE_ENDPOINT_REFERENCE_WIRE_FIELDS: &[ClrWireField] =
    &[ClrWireField::string("Name"), ClrWireField::guid("Id")];
const ENDPOINT_AUTHORIZATION_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("Scheme"),
    ClrWireField::typed("Parameters", ClrWireShape::ExactStringMap),
];
const VARIABLE_VALUE_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("Value"),
    ClrWireField::boolean("IsSecret"),
];
const MASK_HINT_WIRE_FIELDS: &[ClrWireField] =
    &[ClrWireField::raw("Type"), ClrWireField::string("Value")];
const WORKSPACE_WIRE_FIELDS: &[ClrWireField] = &[ClrWireField::string("Clean")];
const ACTIONS_ENVIRONMENT_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("Name"),
    ClrWireField::typed("Url", ClrWireShape::TemplateToken),
];
const DEBUGGER_TUNNEL_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::boolean("Enabled"),
    ClrWireField::string("HostToken"),
    ClrWireField::string("TunnelId"),
    ClrWireField::string("ClusterId"),
];
const REFERENCE_LINK_WIRE_FIELDS: &[ClrWireField] = &[ClrWireField::string("Href")];
const TEMPLATE_PAIR_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::typed("Key", ClrWireShape::TemplateToken),
    ClrWireField::typed("Value", ClrWireShape::TemplateToken),
];
const PIPELINE_CONTEXT_DATA_WIRE_FIELDS: &[ClrWireField] = &[];
const CONTEXT_PAIR_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::string("K"),
    ClrWireField::typed("V", ClrWireShape::PipelineContextData),
];
const ACTION_STEP_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::raw("Type"),
    ClrWireField::guid("Id"),
    ClrWireField::string("Name"),
    ClrWireField::string("DisplayName"),
    ClrWireField::boolean("Enabled"),
    ClrWireField::string("Condition"),
    ClrWireField::typed("ContinueOnError", ClrWireShape::TemplateToken),
    ClrWireField::typed("TimeoutInMinutes", ClrWireShape::TemplateToken),
    ClrWireField::string("ParallelGroupId"),
    ClrWireField::typed("Reference", ClrWireShape::ActionReference),
    ClrWireField::typed("DisplayNameToken", ClrWireShape::TemplateToken),
    ClrWireField::string("ContextName"),
    ClrWireField::typed("Environment", ClrWireShape::TemplateToken),
    ClrWireField::typed("Inputs", ClrWireShape::TemplateToken),
    ClrWireField::boolean("Background"),
];
const BACKGROUND_STEP_WIRE_FIELDS: &[ClrWireField] = &[
    ClrWireField::raw("Type"),
    ClrWireField::guid("Id"),
    ClrWireField::string("Name"),
    ClrWireField::string("DisplayName"),
    ClrWireField::boolean("Enabled"),
    ClrWireField::string("Condition"),
    ClrWireField::typed("ContinueOnError", ClrWireShape::TemplateToken),
    ClrWireField::typed("TimeoutInMinutes", ClrWireShape::TemplateToken),
    ClrWireField::string("ParallelGroupId"),
    ClrWireField::string("ControlType"),
    ClrWireField::typed("DisplayNameToken", ClrWireShape::TemplateToken),
    ClrWireField::typed("StepIds", ClrWireShape::RawArray),
];

fn clr_wire_fields(schema: ClrWireSchema) -> &'static [ClrWireField] {
    use ClrWireSchema as S;
    match schema {
        S::AgentJob => ROOT_WIRE_FIELDS,
        S::Plan => PLAN_WIRE_FIELDS,
        S::Owner => OWNER_WIRE_FIELDS,
        S::Timeline => TIMELINE_WIRE_FIELDS,
        S::JobResources => JOB_RESOURCES_WIRE_FIELDS,
        S::RepositoryResource | S::ContainerResource => RESOURCE_WIRE_FIELDS,
        S::ServiceEndpoint => SERVICE_ENDPOINT_WIRE_FIELDS,
        S::ServiceEndpointReference => SERVICE_ENDPOINT_REFERENCE_WIRE_FIELDS,
        S::EndpointAuthorization => ENDPOINT_AUTHORIZATION_WIRE_FIELDS,
        S::VariableValue => VARIABLE_VALUE_WIRE_FIELDS,
        S::MaskHint => MASK_HINT_WIRE_FIELDS,
        S::Workspace => WORKSPACE_WIRE_FIELDS,
        S::ActionsEnvironment => ACTIONS_ENVIRONMENT_WIRE_FIELDS,
        S::DebuggerTunnel => DEBUGGER_TUNNEL_WIRE_FIELDS,
        S::ReferenceLink => REFERENCE_LINK_WIRE_FIELDS,
        S::TemplatePair => TEMPLATE_PAIR_WIRE_FIELDS,
        S::PipelineContextData => PIPELINE_CONTEXT_DATA_WIRE_FIELDS,
        S::ContextPair => CONTEXT_PAIR_WIRE_FIELDS,
        S::ActionStep => ACTION_STEP_WIRE_FIELDS,
        S::BackgroundStep => BACKGROUND_STEP_WIRE_FIELDS,
    }
}

fn ordered_member<'a>(
    members: &'a [(String, ClrOrderedValue)],
    name: &str,
) -> Option<&'a ClrOrderedValue> {
    members
        .iter()
        .rfind(|(key, _)| key == name)
        .or_else(|| {
            members
                .iter()
                .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
        })
        .map(|(_, value)| value)
}

fn insert_case_insensitive(object: &mut serde_json::Map<String, Value>, key: String, value: Value) {
    if let Some(existing) = object
        .keys()
        .find(|existing| clr_ordinal_ignore_case_eq(existing, &key))
        .cloned()
    {
        object.insert(existing, value);
    } else {
        object.insert(key, value);
    }
}

/// Unicode simple-case comparison for Newtonsoft's OrdinalIgnoreCase maps
/// and typed property lookup. A full uppercase mapping can expand one
/// character into several (for example sharp s), which ordinal comparison
/// does not do.
fn clr_ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    crate::job_message::clr_ordinal_ignore_case_eq(left, right)
}

/// EndpointAuthorization.Parameters and ResourceProperties first deserialize
/// into ordinal dictionaries, then copy into an OrdinalIgnoreCase dictionary
/// during OnDeserialized. The copy rejects distinct keys that compare equal;
/// exact duplicate JSON keys have already taken the last assigned value.
fn validate_clr_case_collision(
    value: &ClrOrderedValue,
) -> std::result::Result<(), ClrValidationError> {
    let ClrOrderedValue::Object(entries) = value else {
        return Ok(());
    };
    for (index, (key, _)) in entries.iter().enumerate() {
        if entries[index + 1..]
            .iter()
            .any(|(later, _)| key != later && clr_ordinal_ignore_case_eq(key, later))
        {
            return Err(clr_serialization_error(
                "case-insensitive dictionary copy contains duplicate keys",
            ));
        }
    }
    Ok(())
}

fn validate_clr_nullable_string_map_occurrences(
    value: &ClrOrderedValue,
    reject_case_collision: bool,
) -> std::result::Result<(), ClrValidationError> {
    let ClrOrderedValue::Object(entries) = value else {
        return Ok(());
    };
    // Newtonsoft materializes each dictionary value in wire order; a bad
    // earlier value throws before a later duplicate can replace it.
    for (_, value) in entries {
        validate_clr_nullable_string_value(&value.clone().into_clr_string_value())?;
    }
    if reject_case_collision {
        validate_clr_case_collision(value)?;
    }
    Ok(())
}

fn validate_clr_merged_case_collisions(
    value: &Value,
    shape: ClrWireShape,
) -> std::result::Result<(), ClrValidationError> {
    match shape {
        ClrWireShape::ExactStringMap => {
            let Some(object) = value.as_object() else {
                return Ok(());
            };
            let keys: Vec<_> = object.keys().collect();
            for (index, key) in keys.iter().enumerate() {
                if keys[index + 1..]
                    .iter()
                    .any(|later| *key != *later && clr_ordinal_ignore_case_eq(key, later))
                {
                    return Err(clr_serialization_error(
                        "case-insensitive dictionary copy contains duplicate keys",
                    ));
                }
            }
            Ok(())
        }
        // These payloads are already encoded as a strict ContextValue tree;
        // their source occurrence rules were checked before normalization.
        ClrWireShape::PropertyBag | ClrWireShape::JTokenObject => Ok(()),
        ClrWireShape::Object(schema) => {
            let Some(object) = value.as_object() else {
                return Ok(());
            };
            for field in clr_wire_fields(schema) {
                if let Some(member) = object.get(field.name) {
                    validate_clr_merged_case_collisions(member, field.shape)?;
                }
            }
            Ok(())
        }
        ClrWireShape::Array(schema) => {
            if let Some(values) = value.as_array() {
                for item in values {
                    validate_clr_merged_case_collisions(item, ClrWireShape::Object(schema))?;
                }
            }
            Ok(())
        }
        ClrWireShape::ArrayTemplateToken => {
            if let Some(values) = value.as_array() {
                for item in values {
                    validate_clr_merged_case_collisions(item, ClrWireShape::TemplateToken)?;
                }
            }
            Ok(())
        }
        ClrWireShape::ArrayPipelineContextData => {
            if let Some(values) = value.as_array() {
                for item in values {
                    validate_clr_merged_case_collisions(item, ClrWireShape::PipelineContextData)?;
                }
            }
            Ok(())
        }
        ClrWireShape::RawArray => Ok(()),
        ClrWireShape::MapValues(schema) => {
            if let Some(values) = value.as_object() {
                for item in values.values() {
                    validate_clr_merged_case_collisions(item, ClrWireShape::Object(schema))?;
                }
            }
            Ok(())
        }
        ClrWireShape::Steps => {
            if let Some(values) = value.as_array() {
                for item in values {
                    let Some(kind) = item
                        .as_object()
                        .and_then(|object| object.get("Type"))
                        .and_then(ordered_step_type_value)
                    else {
                        continue;
                    };
                    let schema = match kind {
                        4 => ClrWireSchema::ActionStep,
                        5 => ClrWireSchema::BackgroundStep,
                        _ => continue,
                    };
                    validate_clr_merged_case_collisions(item, ClrWireShape::Object(schema))?;
                }
            }
            Ok(())
        }
        ClrWireShape::Links => Ok(()),
        ClrWireShape::TemplateToken => validate_clr_merged_template_collisions(value),
        ClrWireShape::PipelineContextData => Ok(()),
        ClrWireShape::ActionReference => Ok(()),
        ClrWireShape::String
        | ClrWireShape::Guid
        | ClrWireShape::Int32
        | ClrWireShape::Int64
        | ClrWireShape::Boolean
        | ClrWireShape::DateTime
        | ClrWireShape::Double
        | ClrWireShape::Raw
        | ClrWireShape::Uri
        | ClrWireShape::CaseInsensitiveStringMap
        | ClrWireShape::ExactNullableStringMap => Ok(()),
    }
}

fn ordered_step_type_value(value: &Value) -> Option<i32> {
    let ordered = match value {
        Value::Number(number) => ClrOrderedValue::Number(ordered_json_number(number)?),
        Value::String(string) => ClrOrderedValue::String(string.clone()),
        _ => return None,
    };
    ordered_step_type(&ordered)
}

fn ordered_json_number(number: &serde_json::Number) -> Option<JsonNumber> {
    let kind = if number.is_f64() {
        JsonNumberKind::Float(number.as_f64()?)
    } else if let Some(value) = number.as_i64() {
        JsonNumberKind::Int64(value)
    } else {
        JsonNumberKind::BigInteger(number.as_u64()?.to_string())
    };
    Some(JsonNumber {
        kind,
        lexeme: number.to_string(),
        origin: JsonReaderOrigin::JObjectReader,
    })
}

fn validate_clr_merged_template_collisions(
    value: &Value,
) -> std::result::Result<(), ClrValidationError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let kind = object.get("Type").and_then(Value::as_i64).unwrap_or(0);
    let child_shape = match kind {
        1 => Some(ClrWireShape::ArrayTemplateToken),
        2 => Some(ClrWireShape::Array(ClrWireSchema::TemplatePair)),
        _ => None,
    };
    if let Some(shape) = child_shape
        && let Some(child) = object.get(if kind == 1 { "Seq" } else { "Map" })
    {
        validate_clr_merged_case_collisions(child, shape)?;
    }
    Ok(())
}

fn merge_clr_wire_value(shape: ClrWireShape, previous: Value, next: Value) -> Value {
    match shape {
        ClrWireShape::Object(schema) => merge_clr_wire_objects(schema, previous, next),
        ClrWireShape::String
        | ClrWireShape::Guid
        | ClrWireShape::Int32
        | ClrWireShape::Int64
        | ClrWireShape::Boolean
        | ClrWireShape::DateTime => next,
        ClrWireShape::Uri => next,
        ClrWireShape::Double => next,
        ClrWireShape::Array(_)
        | ClrWireShape::ArrayTemplateToken
        | ClrWireShape::ArrayPipelineContextData
        | ClrWireShape::Steps
        | ClrWireShape::RawArray => merge_clr_wire_arrays(previous, next),
        ClrWireShape::MapValues(ClrWireSchema::PipelineContextData) => {
            crate::job_message::merge_context_data_pair_arrays(previous, next)
        }
        ClrWireShape::MapValues(_) => merge_clr_wire_maps(previous, next, false),
        ClrWireShape::CaseInsensitiveStringMap => merge_clr_wire_maps(previous, next, true),
        ClrWireShape::ExactStringMap => merge_clr_wire_maps(previous, next, false),
        // ResourcePropertiesJsonConverter creates a new ResourceProperties
        // for each occurrence and ignores existingValue; a repeated
        // Properties member replaces the earlier property bag.
        ClrWireShape::PropertyBag => next,
        ClrWireShape::JTokenObject => next,
        ClrWireShape::ExactNullableStringMap => merge_clr_wire_maps(previous, next, false),
        ClrWireShape::Links => merge_clr_wire_maps(previous, next, false),
        ClrWireShape::PipelineContextData
        | ClrWireShape::TemplateToken
        | ClrWireShape::ActionReference
        | ClrWireShape::Raw => next,
    }
}

fn merge_clr_wire_arrays(previous: Value, next: Value) -> Value {
    match (previous, next) {
        (Value::Array(mut previous), Value::Array(next)) => {
            previous.extend(next);
            Value::Array(previous)
        }
        (_, next) => next,
    }
}

fn merge_clr_wire_maps(previous: Value, next: Value, case_insensitive: bool) -> Value {
    match (previous, next) {
        (Value::Object(mut previous), Value::Object(next)) => {
            for (key, value) in next {
                if case_insensitive {
                    insert_case_insensitive(&mut previous, key, value);
                } else {
                    previous.insert(key, value);
                }
            }
            Value::Object(previous)
        }
        (_, next) => next,
    }
}

fn merge_clr_wire_objects(schema: ClrWireSchema, previous: Value, next: Value) -> Value {
    let (mut previous, next) = match (previous, next) {
        (Value::Object(previous), Value::Object(next)) => (previous, next),
        (_, next) => return next,
    };
    for (key, value) in next {
        let field = clr_wire_fields(schema)
            .iter()
            .find(|field| clr_ordinal_ignore_case_eq(&key, field.name));
        if let (Some(field), Some(old_value)) = (field, previous.remove(&key)) {
            if matches!(schema, ClrWireSchema::EndpointAuthorization) && field.name == "Parameters"
            {
                // OnDeserialized copies m_serializedParameters into a fresh
                // OrdinalIgnoreCase dictionary after each repeated
                // EndpointAuthorization object, but only when the serialized
                // source map has entries. Null and empty maps leave the
                // earlier copied dictionary intact.
                let replace = value
                    .as_object()
                    .is_some_and(|parameters| !parameters.is_empty());
                previous.insert(
                    field.name.to_owned(),
                    if replace { value } else { old_value },
                );
                continue;
            }
            previous.insert(
                field.name.to_owned(),
                merge_clr_wire_value(field.shape, old_value, value),
            );
        } else if let Some(field) = field {
            previous.insert(field.name.to_owned(), value);
        } else {
            previous.insert(key, value);
        }
    }
    Value::Object(previous)
}

fn ordered_i32(value: &ClrOrderedValue) -> Option<i32> {
    match value {
        ClrOrderedValue::Number(number) if !number.is_float() => {
            number.as_i64().and_then(|value| i32::try_from(value).ok())
        }
        _ => None,
    }
}

fn ordered_converter_i32(
    value: &ClrOrderedValue,
    field: &str,
) -> std::result::Result<Option<i32>, ClrValidationError> {
    let ClrOrderedValue::Number(number) = value else {
        return Ok(None);
    };
    if number.is_float() {
        return Ok(None);
    }
    let integer = number
        .as_i64()
        .ok_or_else(|| clr_serialization_error(format!("{field} integer is outside Int32")))?;
    i32::try_from(integer)
        .map(Some)
        .map_err(|_| clr_serialization_error(format!("{field} integer is outside Int32")))
}

fn ordered_step_type(value: &ClrOrderedValue) -> Option<i32> {
    if let Some(value) = ordered_i32(value) {
        return Some(value);
    }
    let ClrOrderedValue::String(value) = value else {
        return None;
    };
    let mut combined = 0;
    for name in value.trim().split(',') {
        let number = if name.trim().eq_ignore_ascii_case("Action") {
            4
        } else if name.trim().eq_ignore_ascii_case("BackgroundStepControl") {
            5
        } else {
            return None;
        };
        combined |= number;
    }
    Some(combined)
}

fn ordered_action_type(value: &ClrOrderedValue) -> Option<i32> {
    if let Some(value) = ordered_i32(value) {
        return Some(value);
    }
    let ClrOrderedValue::String(value) = value else {
        return None;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "repository" => Some(1),
        "containerregistry" => Some(2),
        "script" => Some(3),
        _ => None,
    }
}

fn deserialize_clr_string_map<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<BTreeMap<String, Value>>::deserialize(deserializer)?;
    values
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| {
            clr_json_nullable_string(&value)
                .map(|value| (key, value))
                .map_err(clr_de_error::<D::Error>)
        })
        .collect()
}

fn deserialize_clr_nullable_string_map<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<String, Option<String>>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<BTreeMap<String, Value>>::deserialize(deserializer)?;
    values
        .map(|values| {
            values
                .into_iter()
                .map(|(key, value)| {
                    clr_json_nullable_string(&value)
                        .map(|value| (key, value))
                        .map_err(clr_de_error::<D::Error>)
                })
                .collect()
        })
        .transpose()
}

fn default_clr_endpoint_data() -> Option<BTreeMap<String, Option<String>>> {
    Some(BTreeMap::new())
}

#[derive(Debug, Clone, Copy, Default)]
struct ClrInt64(i64);

impl<'de> Deserialize<'de> for ClrInt64 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Value::deserialize(deserializer)? {
            Value::Number(value) => clr_json_int64_value(&Value::Number(value))
                .map(Self)
                .map_err(serde::de::Error::custom),
            Value::String(value) if value.is_empty() => {
                Err(serde::de::Error::custom("Int64 cannot be null"))
            }
            Value::String(value) => value
                .trim()
                .parse::<i64>()
                .map(Self)
                .map_err(serde::de::Error::custom),
            Value::Bool(value) => Ok(Self(if value { 1 } else { 0 })),
            Value::Null => Err(serde::de::Error::custom("Int64 cannot be null")),
            value => Err(serde::de::Error::custom(format!(
                "cannot convert CLR Int64 from {value}"
            ))),
        }
    }
}

fn clr_json_int64_value(value: &Value) -> std::result::Result<i64, String> {
    match value {
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                return Ok(value);
            }
            if let Some(value) = value.as_u64() {
                return i64::try_from(value).map_err(|error| error.to_string());
            }
            value
                .as_f64()
                .map(f64::round_ties_even)
                .filter(|value| *value >= i64::MIN as f64 && *value < 9_223_372_036_854_775_808.0)
                .map(|value| value as i64)
                .ok_or_else(|| "CLR Int64 value is out of range".to_owned())
        }
        Value::String(value) => value
            .trim()
            .parse::<i64>()
            .map_err(|error| error.to_string()),
        Value::Bool(value) => Ok(if *value { 1 } else { 0 }),
        _ => Err(format!("cannot convert CLR Int64 from {value}")),
    }
}

fn deserialize_clr_u16<'de, D>(deserializer: D) -> std::result::Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(serde::de::Error::custom("UInt16 cannot be null"));
    }
    let integer = clr_json_int64_value(&value).map_err(serde::de::Error::custom)?;
    u16::try_from(integer).map_err(serde::de::Error::custom)
}

fn deserialize_clr_is_ready<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Bool(value) => Ok(value),
        Value::Number(number) => Ok(number.as_i64() != Some(0)),
        Value::String(value) => Ok(!value.eq_ignore_ascii_case("false") && value != "0"),
        // EndpointIsReadyConverter deliberately maps every other token to
        // true, including null, arrays, and objects.
        _ => Ok(true),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ClrGuid(Uuid);

impl<'de> Deserialize<'de> for ClrGuid {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_clr_guid(&value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// Match the string forms accepted by `new Guid(string)`. `Uuid::parse_str`
/// accepts URNs (which CLR Guid rejects) and does not accept parenthesized or
/// X-format GUIDs.
fn parse_clr_guid(value: &str) -> std::result::Result<Uuid, String> {
    let value = value.trim();
    if value
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:uuid:"))
    {
        return Err("URN form is not a CLR Guid string".to_owned());
    }

    let normalized = if value.starts_with('(') && value.ends_with(')') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if let Ok(guid) = Uuid::parse_str(normalized) {
        return Ok(guid);
    }

    // Guid X format: {0xdddddddd,0xdddd,0xdddd,{0xdd,0xdd,...}}.
    let Some(parts) = normalized
        .strip_prefix("{0x")
        .and_then(|value| value.strip_suffix('}'))
    else {
        return Err("invalid CLR Guid string".to_owned());
    };
    let mut fields = parts.splitn(4, ',');
    let data1 = parse_clr_guid_hex(fields.next(), 8)?;
    let data2 = parse_clr_guid_hex(fields.next(), 4)?;
    let data3 = parse_clr_guid_hex(fields.next(), 4)?;
    let Some(data4) = fields.next().and_then(|value| value.strip_prefix("{")) else {
        return Err("invalid CLR Guid X string".to_owned());
    };
    let data4 = data4
        .strip_suffix('}')
        .ok_or_else(|| "invalid CLR Guid X string".to_owned())?;
    let mut bytes = Vec::with_capacity(8);
    for item in data4.split(',') {
        bytes.push(parse_clr_guid_hex(Some(item), 2)? as u8);
    }
    if bytes.len() != 8 {
        return Err("invalid CLR Guid X string".to_owned());
    }

    let mut guid_bytes = [0u8; 16];
    guid_bytes[0..4].copy_from_slice(&(data1 as u32).to_be_bytes());
    guid_bytes[4..6].copy_from_slice(&(data2 as u16).to_be_bytes());
    guid_bytes[6..8].copy_from_slice(&(data3 as u16).to_be_bytes());
    guid_bytes[8..16].copy_from_slice(&bytes);
    Ok(Uuid::from_bytes(guid_bytes))
}

fn parse_clr_guid_hex(value: Option<&str>, width: usize) -> std::result::Result<u64, String> {
    let value = value
        .ok_or_else(|| "invalid CLR Guid X string".to_owned())?
        .trim();
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.is_empty()
        || value.len() > width
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid CLR Guid X string".to_owned());
    }
    u64::from_str_radix(value, 16).map_err(|_| "invalid CLR Guid X string".to_owned())
}

#[derive(Debug, Clone, Copy, Default)]
struct ClrDateTime;

impl<'de> Deserialize<'de> for ClrDateTime {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Value::deserialize(deserializer)? {
            Value::String(value) if value.is_empty() => {
                Err(serde::de::Error::custom("DateTime cannot be null"))
            }
            Value::String(value) if is_clr_datetime(&value) => Ok(Self),
            Value::String(value) if is_clr_microsoft_datetime_out_of_range(&value) => {
                Err(clr_de_error::<D::Error>(clr_serialization_error(
                    "Microsoft DateTime ticks are outside the CLR DateTime range",
                )))
            }
            // JsonTextReader.ReadAsDateTime raises JsonReaderException for
            // malformed date text and non-string token kinds. RawHttpClientBase
            // catches that exception and returns default(T).
            Value::String(_)
            | Value::Number(_)
            | Value::Bool(_)
            | Value::Array(_)
            | Value::Object(_) => Err(clr_de_error::<D::Error>(clr_reader_error(
                "invalid DateTime reader token",
            ))),
            Value::Null => Err(serde::de::Error::custom("DateTime cannot be null")),
        }
    }
}

#[derive(Debug, Default)]
struct ClrUri;

impl<'de> Deserialize<'de> for ClrUri {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        if value.as_deref().is_none_or(is_clr_uri) {
            Ok(Self)
        } else {
            // System.Uri conversion failures are JsonSerializationException,
            // which the runner's raw client does not swallow.
            Err(serde::de::Error::custom("invalid CLR Uri"))
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
enum ClrExpressionValueString {
    #[default]
    NullReference,
    Instance,
}

impl<'de> Deserialize<'de> for ClrExpressionValueString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            // ExpressionValueJsonConverter wraps even a null literal in a
            // non-null ExpressionValue<string>; a missing member remains null.
            Value::Null => Ok(Self::Instance),
            Value::String(value) => {
                if value.len() > 3 && value.starts_with("$[") && value.ends_with(']') {
                    let expression = &value[2..value.len() - 1];
                    if expression.trim().is_empty() {
                        return Err(serde::de::Error::custom(
                            "CLR ExpressionValue expression cannot be empty",
                        ));
                    }
                }
                Ok(Self::Instance)
            }
            Value::Number(_) | Value::Bool(_) => Ok(Self::Instance),
            // ExpressionValueJsonConverter calls serializer.Deserialize<T>
            // after the composite token has already been read. Json.NET then
            // rejects arrays/objects as JsonSerializationException; these are
            // retryable and are not swallowed by RawHttpClientBase.
            Value::Array(_) | Value::Object(_) => Err(clr_de_error::<D::Error>(
                clr_serialization_error("CLR ExpressionValue<string> must be scalar"),
            )),
        }
    }
}

#[derive(Debug, Default)]
struct ClrReferenceLinks;

impl<'de> Deserialize<'de> for ClrReferenceLinks {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        validate_clr_reference_links(&value).map_err(clr_de_error::<D::Error>)?;
        Ok(Self)
    }
}

fn validate_clr_reference_links(value: &Value) -> std::result::Result<(), ClrValidationError> {
    let Value::Object(links) = value else {
        return Err(clr_serialization_error("ReferenceLinks must be an object"));
    };
    for (name, value) in links {
        if name.is_empty() {
            return Err(clr_serialization_error(
                "ReferenceLinks key cannot be empty",
            ));
        }
        match value {
            Value::Object(reference) => validate_clr_reference_link(reference)?,
            Value::Array(references) => {
                for reference in references {
                    match reference {
                        Value::Null => {}
                        Value::Object(reference) => validate_clr_reference_link(reference)?,
                        _ => {
                            return Err(clr_serialization_error(
                                "ReferenceLinks array item must be a ReferenceLink",
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(clr_serialization_error(
                    "ReferenceLinks value must be a ReferenceLink or array",
                ));
            }
        }
    }
    Ok(())
}

fn validate_clr_reference_link(
    reference: &serde_json::Map<String, Value>,
) -> std::result::Result<(), ClrValidationError> {
    if let Some(href) = clr_member(reference, "href") {
        validate_clr_nullable_string_value(href).map_err(|error| error.with_context("href"))?;
    }
    Ok(())
}

/// Accept the ISO and Microsoft date strings emitted by Json.NET's runner
/// formatter plus common invariant `DateTime.TryParse` fallback forms.
/// DateParseHandling.None leaves the token as text before this typed conversion.
/// The .NET fallback accepts more culture forms than Rust's `time` parser;
/// unsupported CLR-valid date text remains a documented reader-boundary gap.
fn is_clr_datetime(value: &str) -> bool {
    use time::{
        format_description, format_description::well_known::Rfc3339, Date, OffsetDateTime,
        PrimitiveDateTime,
    };

    if OffsetDateTime::parse(value, &Rfc3339).is_ok() {
        return true;
    }

    if format_description::parse_borrowed::<1>("[year]-[month padding:none]-[day padding:none]")
        .ok()
        .is_some_and(|format| Date::parse(value, &format).is_ok())
    {
        return true;
    }

    for format in [
        "[month repr:long case_sensitive:false] [day padding:none], [year]",
        "[month repr:short case_sensitive:false] [day padding:none], [year]",
        "[month padding:none]/[day padding:none]/[year]",
        "[year]/[month padding:none]/[day padding:none]",
    ] {
        if format_description::parse_borrowed::<1>(format)
            .ok()
            .is_some_and(|format| Date::parse(value, &format).is_ok())
        {
            return true;
        }
    }

    for format in [
        "[year]-[month padding:none]-[day padding:none]T[hour padding:none]:[minute]:[second]",
        "[year]-[month padding:none]-[day padding:none]T[hour padding:none]:[minute]",
        "[year]-[month padding:none]-[day padding:none]T[hour padding:none]:[minute]:[second].[subsecond]",
        "[year]-[month padding:none]-[day padding:none] [hour padding:none]:[minute]",
        "[year]-[month padding:none]-[day padding:none] [hour padding:none]:[minute]:[second]",
        "[year]-[month padding:none]-[day padding:none] [hour padding:none]:[minute]:[second].[subsecond]",
        "[month repr:long case_sensitive:false] [day padding:none], [year] [hour repr:12 padding:none]:[minute] [period case_sensitive:false]",
        "[month repr:long case_sensitive:false] [day padding:none], [year] [hour repr:12 padding:none]:[minute]:[second] [period case_sensitive:false]",
        "[month padding:none]/[day padding:none]/[year] [hour repr:12 padding:none]:[minute] [period case_sensitive:false]",
        "[month padding:none]/[day padding:none]/[year] [hour repr:12 padding:none]:[minute]:[second] [period case_sensitive:false]",
        "[month padding:none]/[day padding:none]/[year] [hour padding:none]:[minute]",
        "[month padding:none]/[day padding:none]/[year] [hour padding:none]:[minute]:[second]",
        "[month padding:none]/[day padding:none]/[year] [hour padding:none]:[minute]:[second].[subsecond]",
        "[year]/[month padding:none]/[day padding:none] [hour padding:none]:[minute]",
        "[year]/[month padding:none]/[day padding:none] [hour padding:none]:[minute]:[second]",
        "[year]/[month padding:none]/[day padding:none] [hour padding:none]:[minute]:[second].[subsecond]",
    ] {
        if format_description::parse_borrowed::<1>(format)
            .ok()
            .is_some_and(|format| PrimitiveDateTime::parse(value, &format).is_ok())
        {
            return true;
        }
    }

    is_clr_microsoft_datetime(value)
}

/// Match Newtonsoft.Json 13.0.3's Microsoft-date parsing. It skips the first
/// timestamp character when searching for an offset so a negative millisecond
/// value is not mistaken for an offset; its integer parser accepts a leading
/// minus but rejects a leading plus. Tick conversion intentionally wraps just
/// like the unchecked Int64 multiplication/addition in `DateTimeUtils`.
fn is_clr_microsoft_datetime(value: &str) -> bool {
    let Some(ticks) = clr_microsoft_datetime_ticks(value) else {
        return false;
    };

    const MAX_DATETIME_TICKS: i64 = 3_155_378_975_999_999_999;
    (0..=MAX_DATETIME_TICKS).contains(&ticks)
}

fn is_clr_microsoft_datetime_out_of_range(value: &str) -> bool {
    let Some(ticks) = clr_microsoft_datetime_ticks(value) else {
        return false;
    };

    const MAX_DATETIME_TICKS: i64 = 3_155_378_975_999_999_999;
    !(0..=MAX_DATETIME_TICKS).contains(&ticks)
}

fn clr_microsoft_datetime_ticks(value: &str) -> Option<i64> {
    let contents = value
        .strip_prefix("/Date(")
        .and_then(|value| value.strip_suffix(")/"))?;

    let offset = contents
        .char_indices()
        .skip(1)
        .find(|(_, character)| matches!(character, '+' | '-'))
        .map(|(index, _)| index);
    let millis_text = offset.map_or(contents, |index| &contents[..index]);
    let millis = parse_newtonsoft_int64(millis_text)?;

    if let Some(offset) = offset {
        // DateTimeUtils passes the full JSON string to TryReadOffset. Thus its
        // length check includes the trailing `)/`; a three-digit offset gets
        // mistaken for hours plus minutes and fails parsing.
        let full_suffix = &value[("/Date(".len() + offset)..];
        parse_newtonsoft_int32(full_suffix.get(1..3)?)?;
        if full_suffix.encode_utf16().count() > 5 {
            parse_newtonsoft_int32(full_suffix.get(3..5)?)?;
        }
    }

    const INITIAL_JAVASCRIPT_DATE_TICKS: i64 = 621_355_968_000_000_000;
    Some(
        millis
            .wrapping_mul(10_000)
            .wrapping_add(INITIAL_JAVASCRIPT_DATE_TICKS),
    )
}

fn parse_newtonsoft_int64(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<i64>().ok())
        .flatten()
}

fn parse_newtonsoft_int32(value: &str) -> Option<i32> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<i32>().ok())
        .flatten()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrAgentJobRequestMessage {
    #[serde(
        rename = "MessageType",
        alias = "messageType",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    message_type: Option<String>,
    #[serde(rename = "Plan", alias = "plan")]
    plan: Option<ClrTaskOrchestrationPlanReference>,
    #[serde(rename = "Timeline", alias = "timeline")]
    timeline: Option<ClrTimelineReference>,
    #[serde(rename = "JobId", alias = "jobId")]
    job_id: ClrGuid,
    #[serde(
        rename = "JobDisplayName",
        alias = "jobDisplayName",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    job_display_name: Option<String>,
    #[serde(
        rename = "JobName",
        alias = "jobName",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    job_name: Option<String>,
    #[serde(rename = "JobContainer", alias = "jobContainer")]
    job_container: Option<ClrTemplateToken>,
    #[serde(rename = "JobServiceContainers", alias = "jobServiceContainers")]
    job_service_containers: Option<ClrTemplateToken>,
    #[serde(rename = "JobOutputs", alias = "jobOutputs")]
    job_outputs: Option<ClrTemplateToken>,
    #[serde(rename = "RequestId", alias = "requestId")]
    request_id: ClrInt64,
    #[serde(rename = "LockedUntil", alias = "lockedUntil")]
    locked_until: ClrDateTime,
    #[serde(rename = "Resources", alias = "resources")]
    resources: Option<ClrJobResources>,
    #[serde(rename = "ContextData", alias = "contextData")]
    context_data: Option<BTreeMap<String, Option<ClrPipelineContextData>>>,
    #[serde(rename = "Workspace", alias = "workspace")]
    workspace: Option<ClrWorkspaceOptions>,
    #[serde(
        rename = "EnvironmentVariables",
        alias = "environmentVariables",
        deserialize_with = "deserialize_clr_collection"
    )]
    environment_variables: Vec<Option<ClrTemplateToken>>,
    #[serde(
        rename = "Variables",
        alias = "variables",
        deserialize_with = "deserialize_clr_collection"
    )]
    variables: BTreeMap<String, Option<ClrVariableValue>>,
    #[serde(
        rename = "Mask",
        alias = "mask",
        deserialize_with = "deserialize_clr_collection"
    )]
    mask_hints: Vec<Option<ClrMaskHint>>,
    #[serde(
        rename = "Steps",
        alias = "steps",
        deserialize_with = "deserialize_clr_steps"
    )]
    steps: Vec<Option<ClrJobStep>>,
    #[serde(
        rename = "Defaults",
        alias = "defaults",
        deserialize_with = "deserialize_clr_collection"
    )]
    defaults: Vec<Option<ClrTemplateToken>>,
    #[serde(rename = "ActionsEnvironment", alias = "actionsEnvironment")]
    actions_environment: Option<ClrActionsEnvironmentReference>,
    #[serde(rename = "Snapshot", alias = "snapshot")]
    snapshot: Option<ClrTemplateToken>,
    #[serde(
        rename = "BillingOwnerId",
        alias = "billingOwnerId",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    billing_owner_id: Option<String>,
    #[serde(
        rename = "EnableDebugger",
        alias = "enableDebugger",
        deserialize_with = "deserialize_clr_bool"
    )]
    enable_debugger: bool,
    #[serde(rename = "DebuggerTunnel", alias = "debuggerTunnel")]
    debugger_tunnel: Option<ClrDebuggerTunnelInfo>,
    #[serde(
        rename = "DebuggerWelcomeMessage",
        alias = "debuggerWelcomeMessage",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    debugger_welcome_message: Option<String>,
    #[serde(
        rename = "dependencies",
        deserialize_with = "deserialize_clr_string_list"
    )]
    actions_dependencies: Vec<Option<String>>,
    #[serde(
        rename = "FileTable",
        alias = "fileTable",
        deserialize_with = "deserialize_clr_string_list"
    )]
    file_table: Vec<Option<String>>,
    #[serde(
        rename = "JobSidecarContainers",
        alias = "jobSidecarContainers",
        deserialize_with = "deserialize_clr_nullable_string_map"
    )]
    job_sidecar_containers: Option<BTreeMap<String, Option<String>>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrTaskOrchestrationPlanReference {
    #[serde(rename = "ScopeIdentifier", alias = "scopeIdentifier")]
    scope_identifier: ClrGuid,
    #[serde(
        rename = "PlanType",
        alias = "planType",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    plan_type: Option<String>,
    #[serde(rename = "Version", alias = "version")]
    version: ClrInt32,
    #[serde(rename = "PlanId", alias = "planId")]
    plan_id: ClrGuid,
    #[serde(
        rename = "PlanGroup",
        alias = "planGroup",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    plan_group: Option<String>,
    #[serde(rename = "ArtifactUri", alias = "artifactUri")]
    artifact_uri: Option<ClrUri>,
    #[serde(rename = "ArtifactLocation", alias = "artifactLocation")]
    artifact_location: Option<ClrUri>,
    #[serde(rename = "Definition", alias = "definition")]
    definition: Option<ClrTaskOrchestrationOwner>,
    #[serde(rename = "Owner", alias = "owner")]
    owner: Option<ClrTaskOrchestrationOwner>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrTaskOrchestrationOwner {
    #[serde(rename = "Id", alias = "id")]
    id: ClrInt32,
    #[serde(
        rename = "Name",
        alias = "name",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    name: Option<String>,
    #[serde(rename = "_links")]
    links: Option<ClrReferenceLinks>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrTimelineReference {
    #[serde(rename = "Id", alias = "id")]
    id: ClrGuid,
    #[serde(rename = "ChangeId", alias = "changeId")]
    change_id: ClrInt32,
    #[serde(rename = "Location", alias = "location")]
    location: Option<ClrUri>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrRepositoryResource {
    #[serde(
        rename = "Alias",
        alias = "alias",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    alias: Option<String>,
    #[serde(rename = "Endpoint", alias = "endpoint")]
    endpoint: Option<ClrServiceEndpointReference>,
    #[serde(rename = "Properties", alias = "properties")]
    properties: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrContainerResource {
    #[serde(
        rename = "Alias",
        alias = "alias",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    alias: Option<String>,
    #[serde(rename = "Endpoint", alias = "endpoint")]
    endpoint: Option<ClrServiceEndpointReference>,
    #[serde(rename = "Properties", alias = "properties")]
    properties: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrServiceEndpointReference {
    #[serde(rename = "Name", alias = "name")]
    name: ClrExpressionValueString,
    #[serde(rename = "Id", alias = "id")]
    id: ClrGuid,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrServiceEndpoint {
    #[serde(rename = "Id", alias = "id")]
    id: ClrGuid,
    #[serde(rename = "Name", alias = "name")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    name: Option<String>,
    #[serde(rename = "Type", alias = "type")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    endpoint_type: Option<String>,
    #[serde(rename = "Owner", alias = "owner")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    owner: Option<String>,
    #[serde(rename = "Url", alias = "url")]
    url: Option<ClrUri>,
    #[serde(rename = "Description", alias = "description")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    description: Option<String>,
    #[serde(rename = "Authorization", alias = "authorization")]
    authorization: Option<ClrEndpointAuthorization>,
    #[serde(rename = "GroupScopeId", alias = "groupScopeId")]
    group_scope_id: ClrGuid,
    #[serde(
        rename = "Data",
        alias = "data",
        default = "default_clr_endpoint_data",
        deserialize_with = "deserialize_clr_nullable_string_map"
    )]
    data: Option<BTreeMap<String, Option<String>>>,
    #[serde(
        rename = "IsShared",
        alias = "isShared",
        deserialize_with = "deserialize_clr_bool"
    )]
    is_shared: bool,
    #[serde(
        rename = "IsReady",
        alias = "isReady",
        default = "default_clr_is_ready",
        deserialize_with = "deserialize_clr_is_ready"
    )]
    is_ready: bool,
    #[serde(rename = "OperationStatus", alias = "operationStatus")]
    operation_status: Option<BTreeMap<String, Value>>,
}

fn default_clr_is_ready() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrEndpointAuthorization {
    #[serde(
        rename = "Scheme",
        alias = "scheme",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    scheme: Option<String>,
    #[serde(
        rename = "Parameters",
        alias = "parameters",
        deserialize_with = "deserialize_clr_string_map"
    )]
    parameters: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Default)]
struct ClrMaskType(i32);

impl<'de> Deserialize<'de> for ClrMaskType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Value::deserialize(deserializer)? {
            Value::Number(value) if value.is_f64() => Err(serde::de::Error::custom(
                "StringEnumConverter does not accept floating-point MaskType values",
            )),
            Value::Number(value) => value
                .as_i64()
                .map(|value| Self(value as i32))
                .or_else(|| value.as_u64().map(|value| Self(value as u32 as i32)))
                .ok_or_else(|| serde::de::Error::custom("MaskType integer is out of range")),
            Value::String(value) => {
                clr_enum_integer(&Value::String(value), &[("Variable", 1), ("Regex", 2)])
                    .map(|value| value.map(Self))
                    .map_err(serde::de::Error::custom)?
                    .ok_or_else(|| serde::de::Error::custom("invalid MaskType string"))
            }
            _ => Err(serde::de::Error::custom(
                "MaskType must be an integer or name",
            )),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrMaskHint {
    #[serde(rename = "Type", alias = "type")]
    r#type: ClrMaskType,
    #[serde(
        rename = "Value",
        alias = "value",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    value: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrVariableValue {
    #[serde(
        rename = "Value",
        alias = "value",
        deserialize_with = "deserialize_clr_nullable_string"
    )]
    value: Option<String>,
    #[serde(
        rename = "IsSecret",
        alias = "isSecret",
        deserialize_with = "deserialize_clr_bool"
    )]
    is_secret: bool,
}

fn default_clr_step_enabled() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrJobStep {
    #[serde(rename = "Id", alias = "id")]
    id: ClrGuid,
    #[serde(rename = "Name", alias = "name")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    name: Option<String>,
    #[serde(rename = "DisplayName", alias = "displayName")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    display_name: Option<String>,
    #[serde(
        rename = "Enabled",
        alias = "enabled",
        default = "default_clr_step_enabled",
        deserialize_with = "deserialize_clr_bool"
    )]
    enabled: bool,
    #[serde(rename = "Condition", alias = "condition")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    condition: Option<String>,
    #[serde(
        rename = "ContinueOnError",
        alias = "continueOnError",
        deserialize_with = "deserialize_clr_option_template_token"
    )]
    continue_on_error: Option<ClrTemplateToken>,
    #[serde(
        rename = "TimeoutInMinutes",
        alias = "timeoutInMinutes",
        deserialize_with = "deserialize_clr_option_template_token"
    )]
    timeout_in_minutes: Option<ClrTemplateToken>,
    #[serde(rename = "ParallelGroupId", alias = "parallelGroupId")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    parallel_group_id: Option<String>,
}

fn deserialize_clr_option_template_token<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ClrTemplateToken>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<ClrTemplateToken>::deserialize(deserializer)
}

fn step_member<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
        .map(|(_, value)| value)
}

fn clr_enum_integer(
    value: &Value,
    names: &[(&str, i32)],
) -> std::result::Result<Option<i32>, String> {
    match value {
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return i32::try_from(value)
                    .map(Some)
                    .map_err(|_| "enum integer outside Int32".to_owned());
            }
            if let Some(value) = number.as_u64() {
                return i32::try_from(value)
                    .map(Some)
                    .map_err(|_| "enum integer outside Int32".to_owned());
            }
            // Converters dispatch only integer JTokens; floats return null.
            Ok(None)
        }
        Value::String(value) => {
            let mut combined = 0i32;
            for component in value.trim().split(',') {
                let component = component.trim();
                let Some(number) = names
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(component))
                    .map(|(_, value)| *value)
                    .or_else(|| component.parse::<i32>().ok())
                else {
                    return Ok(None);
                };
                combined |= number;
            }
            Ok(Some(combined))
        }
        _ => Ok(None),
    }
}

fn validate_clr_string_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<(), ClrValidationError> {
    if let Some(value) = step_member(object, name) {
        validate_clr_nullable_string_value(value).map_err(|error| error.with_context(name))
    } else {
        Ok(())
    }
}

fn validate_clr_guid_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<(), ClrValidationError> {
    if let Some(value) = step_member(object, name) {
        match value {
            Value::String(value) if parse_clr_guid(value).is_ok() => Ok(()),
            _ => Err(clr_serialization_error(format!(
                "{name} must be a non-null CLR Guid"
            ))),
        }
    } else {
        Ok(())
    }
}

fn validate_clr_template_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<(), ClrValidationError> {
    if let Some(value) = step_member(object, name) {
        validate_clr_template_token(value).map_err(|error| error.with_context(name))
    } else {
        Ok(())
    }
}

fn validate_clr_job_step_base(
    object: &serde_json::Map<String, Value>,
) -> std::result::Result<(), ClrValidationError> {
    validate_clr_guid_field(object, "Id")?;
    for name in ["Name", "DisplayName", "Condition", "ParallelGroupId"] {
        validate_clr_string_field(object, name)?;
    }
    if let Some(value) = step_member(object, "Enabled") {
        validate_clr_bool_value(value, false)?;
    }
    for name in ["ContinueOnError", "TimeoutInMinutes"] {
        validate_clr_template_field(object, name)?;
    }
    Ok(())
}

fn validate_clr_action_step(
    object: &serde_json::Map<String, Value>,
) -> std::result::Result<(), ClrValidationError> {
    validate_clr_job_step_base(object)?;
    if let Some(reference) = step_member(object, "Reference") {
        validate_clr_action_reference(Some(reference))
            .map_err(|error| error.with_context("Reference"))?;
    }
    validate_clr_template_field(object, "DisplayNameToken")?;
    validate_clr_template_field(object, "Environment")?;
    validate_clr_template_field(object, "Inputs")?;
    validate_clr_string_field(object, "ContextName")?;
    if let Some(background) = step_member(object, "Background") {
        validate_clr_bool_value(background, false)?;
    }
    Ok(())
}

fn validate_clr_background_step_control(
    object: &serde_json::Map<String, Value>,
) -> std::result::Result<(), ClrValidationError> {
    validate_clr_job_step_base(object)?;
    validate_clr_string_field(object, "ControlType")?;
    validate_clr_template_field(object, "DisplayNameToken")?;
    if let Some(step_ids) = step_member(object, "StepIds") {
        match step_ids {
            Value::Null => {}
            Value::Array(step_ids) => {
                for step_id in step_ids {
                    validate_clr_nullable_string_value(step_id)?;
                }
            }
            _ => return Err("StepIds must be a CLR string array".into()),
        }
    }
    Ok(())
}

fn validate_clr_action_reference(
    reference: Option<&Value>,
) -> std::result::Result<(), ClrValidationError> {
    let Some(Value::Object(object)) = reference else {
        // The upstream converter returns null when Reference is not an object.
        return Ok(());
    };
    let Some(type_value) = step_member(object, "Type") else {
        return Ok(());
    };
    let Some(action_type) = clr_enum_integer(
        type_value,
        &[("Repository", 1), ("ContainerRegistry", 2), ("Script", 3)],
    )?
    else {
        // Invalid non-integer enum text makes the converter return null.
        return Ok(());
    };
    match action_type {
        1 => {
            for name in ["Name", "Ref", "RepositoryType", "Path"] {
                validate_clr_string_field(object, name)?;
            }
        }
        2 => validate_clr_string_field(object, "Image")?,
        3 => {}
        _ => return Err(format!("unknown ActionSourceType value {action_type}").into()),
    }
    Ok(())
}

fn deserialize_clr_steps<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<Option<ClrJobStep>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(items) = value.as_array() else {
        return if value.is_null() {
            Ok(Vec::new())
        } else {
            Err(serde::de::Error::custom("Steps must be an array"))
        };
    };
    let mut steps = Vec::with_capacity(items.len());
    for item in items {
        let Some(object) = item.as_object() else {
            // StepConverter returns null for non-object entries.
            steps.push(None);
            continue;
        };
        let Some(type_value) = step_member(object, "Type") else {
            steps.push(None);
            continue;
        };
        let Some(step_type) =
            clr_enum_integer(type_value, &[("Action", 4), ("BackgroundStepControl", 5)])
                .map_err(serde::de::Error::custom)?
        else {
            // The converter returns null for non-integer values that are not
            // enum strings.
            steps.push(None);
            continue;
        };
        match step_type {
            4 => validate_clr_action_step(object),
            5 => validate_clr_background_step_control(object),
            _ => {
                return Err(serde::de::Error::custom(format!(
                    "unknown StepType value {step_type}"
                )));
            }
        }
        .map_err(clr_de_error::<D::Error>)?;
        let step = ClrJobStep::deserialize(ClrValueDeserializer::<D::Error>::new(item.clone()))?;
        steps.push(Some(step));
    }
    Ok(steps)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrJobResources {
    #[serde(
        rename = "Endpoints",
        alias = "endpoints",
        deserialize_with = "deserialize_clr_collection"
    )]
    endpoints: Vec<Option<ClrServiceEndpoint>>,
    #[serde(
        rename = "Repositories",
        alias = "repositories",
        deserialize_with = "deserialize_clr_collection"
    )]
    repositories: Vec<Option<ClrRepositoryResource>>,
    #[serde(
        rename = "Containers",
        alias = "containers",
        deserialize_with = "deserialize_clr_collection"
    )]
    containers: Vec<Option<ClrContainerResource>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrWorkspaceOptions {
    #[serde(rename = "Clean", alias = "clean")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    clean: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrActionsEnvironmentReference {
    #[serde(rename = "Name", alias = "name")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    name: Option<String>,
    #[serde(rename = "Url", alias = "url")]
    url: Option<ClrTemplateToken>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClrDebuggerTunnelInfo {
    #[serde(rename = "TunnelId", alias = "tunnelId")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    tunnel_id: Option<String>,
    #[serde(rename = "ClusterId", alias = "clusterId")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    cluster_id: Option<String>,
    #[serde(rename = "HostToken", alias = "hostToken")]
    #[serde(deserialize_with = "deserialize_clr_nullable_string")]
    host_token: Option<String>,
    #[serde(
        rename = "Port",
        alias = "port",
        deserialize_with = "deserialize_clr_u16"
    )]
    port: u16,
}

fn clr_validation_from_wire(error: ClrWireError) -> ClrValidationError {
    match error {
        ClrWireError::Reader(message) => ClrValidationError::Reader(message),
        ClrWireError::Serialization(message) => ClrValidationError::Serialization(message),
    }
}

/// Validate every wire occurrence before collapsing duplicates into the
/// runtime `Value`. Json.NET processes ordinary CLR members in source order;
/// a bad earlier value still throws even when a later duplicate would replace
/// it. Converter-backed JToken shapes are handled by their converter path,
/// whose JObject projection uses the final duplicate member.
fn validate_clr_ordered_occurrences(
    value: &ClrOrderedValue,
    shape: ClrWireShape,
) -> std::result::Result<(), ClrValidationError> {
    if matches!(value, ClrOrderedValue::Constructor { .. }) && !matches!(shape, ClrWireShape::Raw) {
        return Err(clr_serialization_error(
            "Json.NET constructor token cannot deserialize into this DTO shape",
        ));
    }
    match shape {
        ClrWireShape::Raw => Ok(()),
        ClrWireShape::String if matches!(value, ClrOrderedValue::Undefined) => {
            Err(clr_reader_error("undefined cannot be read as a CLR string"))
        }
        ClrWireShape::String => {
            validate_clr_nullable_string_value(&value.clone().into_clr_string_value())
        }
        ClrWireShape::Guid => match value {
            ClrOrderedValue::NonFinite { .. } => {
                Err(clr_serialization_error("invalid CLR Guid value"))
            }
            _ => Ok(()),
        },
        ClrWireShape::Int32
        | ClrWireShape::Int64
        | ClrWireShape::Boolean
        | ClrWireShape::DateTime => match value {
            ClrOrderedValue::Undefined => Err(clr_reader_error(
                "undefined cannot be read as a CLR primitive value",
            )),
            ClrOrderedValue::NonFinite { .. } => {
                Err(clr_reader_error("invalid nonfinite CLR primitive value"))
            }
            _ => Ok(()),
        },
        ClrWireShape::Double => match value {
            ClrOrderedValue::Undefined => {
                Err(clr_reader_error("undefined cannot be read as a CLR Double"))
            }
            _ => validate_clr_double_value(&value.clone().into_clr_value(ClrWireShape::Double)),
        },
        ClrWireShape::CaseInsensitiveStringMap => {
            validate_clr_nullable_string_map_occurrences(value, false)
        }
        ClrWireShape::Uri => match value {
            ClrOrderedValue::Null | ClrOrderedValue::Undefined => Ok(()),
            ClrOrderedValue::NonFinite { .. } => {
                Err(clr_serialization_error("invalid CLR Uri value"))
            }
            _ => validate_clr_deserialize::<Option<ClrUri>>(value.clone().into_value()),
        },
        ClrWireShape::ExactStringMap => validate_clr_nullable_string_map_occurrences(value, true),
        ClrWireShape::PropertyBag => validate_clr_case_collision(value),
        ClrWireShape::JTokenObject => match value {
            ClrOrderedValue::Null | ClrOrderedValue::Object(_) => Ok(()),
            _ => Err(clr_serialization_error("OperationStatus must be a JObject")),
        },
        ClrWireShape::ExactNullableStringMap => {
            validate_clr_nullable_string_map_occurrences(value, false)
        }
        ClrWireShape::Object(schema) => {
            if matches!(value, ClrOrderedValue::Null | ClrOrderedValue::Undefined) {
                Ok(())
            } else {
                validate_clr_object_occurrences(value, schema)
            }
        }
        ClrWireShape::Array(schema) => {
            if let ClrOrderedValue::Array(values) = value {
                for value in values {
                    if matches!(value, ClrOrderedValue::Null | ClrOrderedValue::Undefined) {
                        continue;
                    }
                    validate_clr_object_occurrences(value, schema)?;
                }
            }
            Ok(())
        }
        ClrWireShape::ArrayTemplateToken => {
            if let ClrOrderedValue::Array(values) = value {
                for value in values {
                    validate_clr_ordered_occurrences(value, ClrWireShape::TemplateToken)?;
                }
            }
            Ok(())
        }
        ClrWireShape::ArrayPipelineContextData => {
            if let ClrOrderedValue::Array(values) = value {
                for value in values {
                    validate_clr_ordered_occurrences(value, ClrWireShape::PipelineContextData)?;
                }
            }
            Ok(())
        }
        ClrWireShape::MapValues(schema) => {
            let ClrOrderedValue::Object(values) = value else {
                if matches!(schema, ClrWireSchema::PipelineContextData)
                    && !matches!(value, ClrOrderedValue::Null)
                {
                    return Err(clr_serialization_error(
                        "ContextData must be an object or null",
                    ));
                }
                return Ok(());
            };
            for (_, value) in values {
                if matches!(value, ClrOrderedValue::Null | ClrOrderedValue::Undefined) {
                    continue;
                }
                if matches!(schema, ClrWireSchema::PipelineContextData) {
                    validate_clr_ordered_occurrences(value, ClrWireShape::PipelineContextData)?;
                } else {
                    validate_clr_object_occurrences(value, schema)?;
                }
            }
            Ok(())
        }
        ClrWireShape::Links => {
            if let ClrOrderedValue::Object(values) = value {
                for (_, value) in values {
                    match value {
                        ClrOrderedValue::Array(values) => {
                            for value in values {
                                if !matches!(
                                    value,
                                    ClrOrderedValue::Null | ClrOrderedValue::Undefined
                                ) {
                                    validate_clr_object_occurrences(
                                        value,
                                        ClrWireSchema::ReferenceLink,
                                    )?;
                                }
                            }
                        }
                        ClrOrderedValue::Object(_) => {
                            validate_clr_object_occurrences(value, ClrWireSchema::ReferenceLink)?
                        }
                        ClrOrderedValue::Undefined => {}
                        _ => {}
                    }
                }
            }
            Ok(())
        }
        ClrWireShape::TemplateToken => validate_clr_template_occurrences(value),
        ClrWireShape::PipelineContextData => validate_clr_context_occurrences(value),
        ClrWireShape::Steps => {
            if let ClrOrderedValue::Array(values) = value {
                for value in values {
                    let collapsed = value.clone().collapse_exact_properties();
                    let ClrOrderedValue::Object(fields) = &collapsed else {
                        continue;
                    };
                    let schema = match ordered_member(fields, "Type").and_then(ordered_step_type) {
                        Some(4) => Some(ClrWireSchema::ActionStep),
                        Some(5) => Some(ClrWireSchema::BackgroundStep),
                        _ => None,
                    };
                    if let Some(schema) = schema {
                        validate_clr_object_occurrences(&collapsed, schema)?;
                    }
                }
            }
            Ok(())
        }
        ClrWireShape::RawArray => Ok(()),
        ClrWireShape::ActionReference => validate_clr_action_reference_occurrences(value),
    }
}

fn validate_clr_object_occurrences(
    value: &ClrOrderedValue,
    schema: ClrWireSchema,
) -> std::result::Result<(), ClrValidationError> {
    if matches!(value, ClrOrderedValue::Constructor { .. }) {
        return Err(clr_serialization_error(
            "Json.NET constructor token cannot deserialize into a CLR object",
        ));
    }
    if matches!(value, ClrOrderedValue::Null | ClrOrderedValue::Undefined) {
        return Ok(());
    }
    let ClrOrderedValue::Object(values) = value else {
        return validate_clr_schema_value(schema, value.clone().into_value());
    };
    for (key, value) in values {
        let Some(field) = clr_wire_fields(schema)
            .iter()
            .find(|field| clr_ordinal_ignore_case_eq(key, field.name))
        else {
            continue;
        };
        validate_clr_ordered_occurrences(value, field.shape)
            .map_err(|error| error.with_context(field.name))?;
        let normalized = value.clone().into_clr_value(field.shape);
        let mut single = serde_json::Map::new();
        single.insert(field.name.to_owned(), normalized);
        validate_clr_schema_value(schema, Value::Object(single))
            .map_err(|error| error.with_context(field.name))?;
    }
    let normalized = value.clone().into_clr_value(ClrWireShape::Object(schema));
    validate_clr_merged_case_collisions(&normalized, ClrWireShape::Object(schema))?;
    validate_clr_schema_value(schema, normalized)
}

fn validate_clr_schema_value(
    schema: ClrWireSchema,
    value: Value,
) -> std::result::Result<(), ClrValidationError> {
    use ClrWireSchema as S;
    match schema {
        S::AgentJob => {
            let mut value = value;
            project_ordered_context_data_for_clr(&mut value)?;
            validate_clr_deserialize::<ClrAgentJobRequestMessage>(value)
        }
        S::Plan => validate_clr_deserialize::<ClrTaskOrchestrationPlanReference>(value),
        S::Owner => validate_clr_deserialize::<ClrTaskOrchestrationOwner>(value),
        S::Timeline => validate_clr_deserialize::<ClrTimelineReference>(value),
        S::JobResources => validate_clr_deserialize::<ClrJobResources>(value),
        S::RepositoryResource => validate_clr_deserialize::<ClrRepositoryResource>(value),
        S::ContainerResource => validate_clr_deserialize::<ClrContainerResource>(value),
        S::ServiceEndpoint => validate_clr_deserialize::<ClrServiceEndpoint>(value),
        S::ServiceEndpointReference => {
            validate_clr_deserialize::<ClrServiceEndpointReference>(value)
        }
        S::EndpointAuthorization => validate_clr_deserialize::<ClrEndpointAuthorization>(value),
        S::VariableValue => validate_clr_deserialize::<ClrVariableValue>(value),
        S::MaskHint => validate_clr_deserialize::<ClrMaskHint>(value),
        S::Workspace => validate_clr_deserialize::<ClrWorkspaceOptions>(value),
        S::ActionsEnvironment => validate_clr_deserialize::<ClrActionsEnvironmentReference>(value),
        S::DebuggerTunnel => validate_clr_deserialize::<ClrDebuggerTunnelInfo>(value),
        S::ReferenceLink => {
            let Some(object) = value.as_object() else {
                return Err(clr_serialization_error("ReferenceLink must be an object"));
            };
            validate_clr_reference_link(object)
        }
        S::TemplatePair => {
            let envelope = json!({"Type": 2, "Map": [value]});
            validate_clr_deserialize::<ClrTemplateToken>(envelope)
        }
        S::PipelineContextData => validate_clr_deserialize::<ClrPipelineContextData>(value),
        S::ContextPair => {
            let envelope = json!({"T": 2, "D": [value]});
            validate_clr_deserialize::<ClrPipelineContextData>(envelope)
        }
        S::ActionStep => {
            let Some(object) = value.as_object() else {
                return Err(clr_serialization_error("ActionStep must be an object"));
            };
            validate_clr_action_step(object)?;
            validate_clr_deserialize::<ClrJobStep>(value)
        }
        S::BackgroundStep => {
            let Some(object) = value.as_object() else {
                return Err(clr_serialization_error(
                    "BackgroundStepControl must be an object",
                ));
            };
            validate_clr_background_step_control(object)?;
            validate_clr_deserialize::<ClrJobStep>(value)
        }
    }
}

fn project_ordered_context_data_for_clr(
    value: &mut Value,
) -> std::result::Result<(), ClrValidationError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    let Some(name) = object
        .keys()
        .find(|name| clr_ordinal_ignore_case_eq(name, "ContextData"))
        .cloned()
    else {
        return Ok(());
    };
    let Some(Value::Array(pairs)) = object.get(&name) else {
        return Ok(());
    };

    let mut context = serde_json::Map::new();
    for pair in pairs {
        let Some(values) = pair.as_array().filter(|values| values.len() == 2) else {
            return Err(clr_serialization_error(
                "ordered ContextData pair must contain two values",
            ));
        };
        let Some(key) = values[0].as_str() else {
            return Err(clr_serialization_error(
                "ordered ContextData key must be a string",
            ));
        };
        context.insert(key.to_owned(), values[1].clone());
    }
    object.insert(name, Value::Object(context));
    Ok(())
}

fn validate_clr_deserialize<T: DeserializeOwned>(
    value: Value,
) -> std::result::Result<(), ClrValidationError> {
    deserialize_clr_value::<T>(value)
        .map(|_| ())
        .map_err(clr_validation_from_wire)
}

#[derive(Clone, Copy)]
enum ClrConverterValidator {
    TemplateToken,
    PipelineContextData,
    ActionReference,
}

/// Validate each selected typed member before case-insensitive aliases are
/// folded into one converter value. Json.NET's serializer visits those JObject
/// members in source order, so an invalid earlier alias still fails even when
/// a later alias would replace it in the normalized DTO.
fn validate_clr_selected_converter_occurrences(
    values: &[(String, ClrOrderedValue)],
    fields: &[ClrWireField],
    discriminator: &'static str,
    kind: i32,
    validator: ClrConverterValidator,
) -> std::result::Result<(), ClrValidationError> {
    for (source_name, value) in values {
        let Some(field) = fields
            .iter()
            .find(|field| clr_ordinal_ignore_case_eq(source_name, field.name))
        else {
            continue;
        };
        if matches!(field.shape, ClrWireShape::Raw) {
            continue;
        }

        validate_clr_ordered_occurrences(value, field.shape)
            .map_err(|error| error.with_context(source_name))?;
        let mut envelope = serde_json::Map::new();
        envelope.insert(
            field.name.to_owned(),
            value.clone().into_clr_value(field.shape),
        );
        envelope.insert(discriminator.to_owned(), Value::from(kind));
        let envelope = Value::Object(envelope);
        let result = match validator {
            ClrConverterValidator::TemplateToken => {
                validate_clr_deserialize::<ClrTemplateToken>(envelope)
            }
            ClrConverterValidator::PipelineContextData => {
                validate_clr_deserialize::<ClrPipelineContextData>(envelope)
            }
            ClrConverterValidator::ActionReference => {
                validate_clr_action_reference(Some(&envelope))
            }
        };
        result.map_err(|error| error.with_context(source_name))?;
    }
    Ok(())
}

fn is_ordered_big_integer(value: &ClrOrderedValue) -> bool {
    matches!(value, ClrOrderedValue::Number(number) if number.big_integer_decimal().is_some())
}

fn validate_clr_template_occurrences(
    value: &ClrOrderedValue,
) -> std::result::Result<(), ClrValidationError> {
    let mut collapsed = value.clone().collapse_exact_properties();
    if matches!(collapsed, ClrOrderedValue::Object(_)) {
        collapsed.set_number_origin(JsonReaderOrigin::JObjectReader);
    }
    if is_ordered_big_integer(&collapsed) {
        // TemplateTokenJsonConverter casts integer tokens to Int64 before
        // constructing NumberToken; Newtonsoft BigInteger cannot be cast.
        return Err(clr_serialization_error(
            "TemplateToken integer scalar is outside Int64",
        ));
    }
    let value = &collapsed;
    if let ClrOrderedValue::Object(fields) = value {
        let kind = match ordered_member(fields, "type") {
            Some(value) => ordered_converter_i32(value, "TemplateToken type")?,
            None => Some(0),
        };
        if let Some(kind) = kind {
            let selected_fields = clr_template_token_fields(kind);
            validate_clr_selected_converter_occurrences(
                fields,
                &selected_fields,
                "Type",
                kind,
                ClrConverterValidator::TemplateToken,
            )?;
        }
    }
    let value = value.clone().into_clr_value(ClrWireShape::TemplateToken);
    validate_clr_deserialize::<ClrTemplateToken>(value)
}

fn validate_clr_context_occurrences(
    value: &ClrOrderedValue,
) -> std::result::Result<(), ClrValidationError> {
    let mut collapsed = value.clone().collapse_exact_properties();
    if matches!(collapsed, ClrOrderedValue::Object(_)) {
        collapsed.set_number_origin(JsonReaderOrigin::JObjectReader);
    }
    if is_ordered_big_integer(&collapsed) {
        // PipelineContextDataJsonConverter casts integer tokens to Int64
        // before converting them to Double, so BigInteger scalar values fail.
        return Err(clr_serialization_error(
            "PipelineContextData integer scalar is outside Int64",
        ));
    }
    let value = &collapsed;
    if let ClrOrderedValue::Object(fields) = value {
        let kind = match ordered_member(fields, "t") {
            Some(value) => ordered_converter_i32(value, "PipelineContextData type")?,
            None => Some(0),
        };
        if let Some(kind) = kind {
            let selected_fields = clr_context_data_fields(kind);
            validate_clr_selected_converter_occurrences(
                fields,
                &selected_fields,
                "T",
                kind,
                ClrConverterValidator::PipelineContextData,
            )?;
        }
    }
    let value = value
        .clone()
        .into_clr_value(ClrWireShape::PipelineContextData);
    validate_clr_deserialize::<ClrPipelineContextData>(value)
}

fn validate_clr_action_reference_occurrences(
    value: &ClrOrderedValue,
) -> std::result::Result<(), ClrValidationError> {
    let mut collapsed = value.clone().collapse_exact_properties();
    if matches!(collapsed, ClrOrderedValue::Object(_)) {
        collapsed.set_number_origin(JsonReaderOrigin::JObjectReader);
    }
    let value = &collapsed;
    if let ClrOrderedValue::Object(fields) = value {
        let kind = ordered_member(fields, "Type").and_then(ordered_action_type);
        if let Some(kind) = kind {
            let selected_fields = clr_action_reference_fields(kind);
            validate_clr_selected_converter_occurrences(
                fields,
                &selected_fields,
                "Type",
                kind,
                ClrConverterValidator::ActionReference,
            )?;
        }
    }
    let value = value.clone().into_clr_value(ClrWireShape::ActionReference);
    if let Value::Object(object) = &value {
        validate_clr_action_reference(Some(&Value::Object(object.clone())))?;
    }
    Ok(())
}

/// Decode one acquired CLR-shaped `AgentJobRequestMessage`. The raw wire value
/// remains available for quarantine and recovery, while this nullable DTO
/// retains null collection entries and reference-property nulls until the
/// runner has durably settled the acquired identity. Runtime admission happens
/// later and must never become an HTTP/JSON acquire retry. The private CLR DTO
/// validates source serializer behavior; the public wire DTO retains values
/// consumed by local runtime materialization.
/// Upstream returns `default(AgentJobRequestMessage)` (null) when there is no
/// JSON body or when its JSON reader rejects the body; a non-object JSON value
/// is a typed deserialization failure and therefore remains retryable.
#[derive(Debug)]
pub struct AcquiredJobPayload {
    /// Untouched response value for local quarantine and diagnostics.
    pub raw: Value,
    /// Exact response text. `Value` cannot preserve duplicate object members.
    pub raw_json: String,
    /// Effective, explicitly supplied identity after CLR case-insensitive
    /// member assignment. Missing GUID members remain `None` here even though
    /// the materialized CLR-shaped message carries `Guid.Empty` defaults.
    pub identity: AcquiredJobIdentity,
    /// Parsed normalized wire message. `None` means CLR null-success or that
    /// the validated wire payload could not be represented by Velnor's wire
    /// DTO; `identity` distinguishes an addressable post-validation parse
    /// failure from a CLR reader-null/default result.
    pub message: Option<crate::job_message::WireAgentJobRequestMessage>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AcquiredJobIdentity {
    pub job_id: Option<String>,
    pub plan_id: Option<String>,
}

fn decode_acquire_job_success_body(
    http_status: u16,
    content_length: Option<u64>,
    content_type: Option<&str>,
    body: &str,
) -> std::result::Result<AcquiredJobPayload, serde_json::Error> {
    if !is_acquire_job_json_response(http_status, content_length, content_type) {
        return Ok(AcquiredJobPayload {
            raw: Value::Null,
            raw_json: body.to_owned(),
            identity: AcquiredJobIdentity::default(),
            message: None,
        });
    }

    let ordered = match parse_clr_ordered_json(body) {
        Ok(ClrOrderedValue::Null) => {
            return Ok(AcquiredJobPayload {
                raw: Value::Null,
                raw_json: body.to_owned(),
                identity: AcquiredJobIdentity::default(),
                message: None,
            });
        }
        Ok(value) => value,
        Err(error)
            if matches!(
                error.classify(),
                serde_json::error::Category::Syntax | serde_json::error::Category::Eof
            ) =>
        {
            // RawHttpClientBase catches JsonReaderException and returns
            // default(T), which is null for AgentJobRequestMessage.
            return Ok(AcquiredJobPayload {
                raw: Value::Null,
                raw_json: body.to_owned(),
                identity: AcquiredJobIdentity::default(),
                message: None,
            });
        }
        Err(error) => return Err(error),
    };

    let raw = ordered.clone().into_value();
    if let Err(error) =
        validate_clr_ordered_occurrences(&ordered, ClrWireShape::Object(ClrWireSchema::AgentJob))
    {
        if error.is_reader_error() {
            return Ok(AcquiredJobPayload {
                raw,
                raw_json: body.to_owned(),
                identity: AcquiredJobIdentity::default(),
                message: None,
            });
        }
        return Err(serde::de::Error::custom(error));
    }
    let normalized = ordered
        .clone()
        .into_clr_value(ClrWireShape::Object(ClrWireSchema::AgentJob));
    if !normalized.is_object() {
        return Err(serde::de::Error::custom(
            "expected AgentJobRequestMessage object",
        ));
    }

    // Keep the upstream contract boundary independent from Velnor's post-
    // acquire runtime model. JsonReaderException from a typed Json.NET reader
    // follows RawHttpClientBase's null-success path; typed serialization
    // failures remain retryable.
    let mut clr_normalized = normalized.clone();
    project_ordered_context_data_for_clr(&mut clr_normalized).map_err(serde::de::Error::custom)?;
    match deserialize_clr_value::<ClrAgentJobRequestMessage>(clr_normalized) {
        Ok(_) => {}
        Err(ClrWireError::Reader(_)) => {
            // Preserve the parsed wire value for local quarantine while
            // matching RawHttpClientBase's default(T) typed result.
            return Ok(AcquiredJobPayload {
                raw,
                raw_json: body.to_owned(),
                identity: AcquiredJobIdentity::default(),
                message: None,
            });
        }
        Err(ClrWireError::Serialization(message)) => {
            return Err(serde::de::Error::custom(message));
        }
    }
    let identity = acquired_job_identity(&normalized);
    crate::job_message::WireAgentJobRequestMessage::validate_deserialization_callback_from_normalized_value(
        &normalized,
    )
    .map_err(|error| {
        <serde_json::Error as serde::de::Error>::custom(format!(
            "Actions Runner OnDeserialized callback failed: {error}"
        ))
    })?;
    let message = crate::job_message::WireAgentJobRequestMessage::from_ordered_normalized_value(
        normalized.clone(),
    );
    Ok(AcquiredJobPayload {
        raw,
        raw_json: body.to_owned(),
        identity,
        message: message.ok(),
    })
}

fn acquired_job_identity_object_member<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key.as_str(), name))
            .map(|(_, value)| value)
    })
}

fn acquired_job_identity(value: &Value) -> AcquiredJobIdentity {
    let guid = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .and_then(|value| parse_clr_guid(value).ok())
            .map(|value| value.hyphenated().to_string())
    };
    let job_id = value
        .as_object()
        .and_then(|object| acquired_job_identity_object_member(object, "JobId"))
        .and_then(|value| guid(Some(value)));
    let plan_id = value
        .as_object()
        .and_then(|object| acquired_job_identity_object_member(object, "Plan"))
        .and_then(Value::as_object)
        .and_then(|object| acquired_job_identity_object_member(object, "PlanId"))
        .and_then(|value| guid(Some(value)));
    AcquiredJobIdentity { job_id, plan_id }
}

fn acquire_job_skip_reason(body: &str) -> Option<AcquireJobSkipReason> {
    match run_service_error_code(body)? {
        404 => Some(AcquireJobSkipReason::NotFound),
        409 => Some(AcquireJobSkipReason::AlreadyAcquired),
        422 => Some(AcquireJobSkipReason::Unprocessable),
        _ => None,
    }
}

/// Whether a typed `acquirejob` reply proves that the broker message is gone.
///
/// Only typed 404 is proof. Typed 409 has unknown ownership; typed 422 is a
/// distinct unprocessable response and does not establish that the message is
/// gone. Both leave the provisional row for the `renewjob` oracle.
#[must_use]
pub fn acquire_reply_is_definitely_gone(body: &str) -> bool {
    run_service_error_code(body) == Some(404)
}

/// Boundary classifier for a non-retriable `acquirejob` reply, produced
/// where the reply is observed and carried on
/// [`AcquireJobOutcome::Skipped`]. Only a typed 404 is
/// [`BrokerErrorCategory::Terminal`] (the slot may be freed now); typed 409
/// and 422 remain [`BrokerErrorCategory::Conflict`] under Velnor's local
/// ownership-safety policy, which retains the provisional intent for the
/// `renewjob` oracle. actions/runner only defines these as non-retriable
/// acquire exceptions; it does not define Velnor's intent-retention rule. The
/// 422 reason remains distinct so callers do not collapse it into a missing
/// message. Untyped or foreign-sourced bodies are retried before this
/// classifier is reached.
pub(crate) fn classify_acquire_skipped(body: &str) -> BrokerErrorCategory {
    match acquire_job_skip_reason(body) {
        Some(AcquireJobSkipReason::NotFound) => BrokerErrorCategory::Terminal,
        Some(AcquireJobSkipReason::AlreadyAcquired | AcquireJobSkipReason::Unprocessable)
        | None => BrokerErrorCategory::Conflict,
    }
}

/// Whether a `renewjob` failure proves the run service has no such job for this
/// runner.
///
/// Only a typed `404` counts. Every other failure — transport, 5xx, auth, or a
/// body that is not a run-service error at all — leaves ownership unproven, and
/// the caller must treat it as indeterminate rather than dropping a row it may
/// still own.
#[must_use]
pub fn renew_failure_is_job_gone(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .is_some_and(|api| is_run_service_job_not_found(&api.body))
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

fn parse_runner_lookup_response(
    response: GithubHttpResponse,
    expected_id: i64,
) -> Result<Option<ListedRunner>> {
    if response.status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&response.status) {
        return Err(github_error_from_response("runner lookup", response));
    }
    let runner: ListedRunner =
        serde_json::from_str(response.body.trim()).context("parse runner lookup response")?;
    if runner.id != Some(expected_id) {
        bail!(
            "runner lookup identity mismatch: requested id {expected_id}, returned {:?}",
            runner.id
        );
    }
    Ok(Some(runner))
}

impl BrokerClient {
    pub fn new(server_url_v2: &str, bearer_token: impl Into<String>) -> Result<Self> {
        validate_authenticated_url(server_url_v2)?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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
            return Err(github_api_error_categorized(
                "create broker session",
                status,
                text,
                classify_broker_session_create_error(status),
            ));
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
            return Err(github_api_error_categorized(
                "acknowledge broker runner request",
                http_status,
                text,
                classify_broker_ack_error(http_status),
            ));
        }
        Ok(())
    }
}

/// Boundary classifier for a failed broker `acknowledge` POST. The only
/// caller is best-effort by design — the ack is a Busy marker, the broker
/// redelivers regardless, and a duplicate ack is safe — so the category is
/// forensic, not a retry input: no retry, one attempt, 30 s timeout.
fn classify_broker_ack_error(status: u16) -> BrokerErrorCategory {
    match status {
        409 => BrokerErrorCategory::Conflict,
        status if is_retriable_completion_status(status) => BrokerErrorCategory::Transient,
        _ => BrokerErrorCategory::Terminal,
    }
}

/// Boundary classifier for a failed broker `create session` POST, read by
/// `create_broker_session_with_retry`. A deterministic refusal is `Terminal`
/// (fixed credentials fail identically on every attempt: fail fast); a 409
/// is `Conflict` (a lost response may already have created the session, so
/// the retry converges instead of abandoning); anything transport-shaped is
/// `Transient`. Same status shape as the ack classifier, separate function
/// because the retry policy it drives differs (retry vs never-retry).
fn classify_broker_session_create_error(status: u16) -> BrokerErrorCategory {
    match status {
        409 => BrokerErrorCategory::Conflict,
        status if is_retriable_completion_status(status) => BrokerErrorCategory::Transient,
        _ => BrokerErrorCategory::Terminal,
    }
}

#[derive(Clone)]
pub struct RunServiceClient {
    http: Client,
    bearer_token: String,
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    acquire_retry_delay_override: Option<Duration>,
    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    complete_retry_delay_override: Option<Duration>,
}

#[derive(Debug)]
pub enum AcquireJobOutcome {
    Acquired(Box<AcquiredJobPayload>),
    Skipped {
        status: StatusCode,
        request_id: Option<String>,
        body: String,
        /// Typed run-service response code that caused upstream to stop
        /// retrying this failed acquire response.
        reason: AcquireJobSkipReason,
        /// Velnor-local boundary category for intent handling:
        /// [`BrokerErrorCategory::Terminal`] for typed 404 only;
        /// [`BrokerErrorCategory::Conflict`] for typed 409/422. Callers match
        /// on the category and the distinct typed reason; they never
        /// re-derive either from `body`.
        category: BrokerErrorCategory,
    },
}

#[derive(Debug, thiserror::Error)]
enum AcquireJobError {
    #[error("transient run-service acquire failure after retries: {0:#}")]
    Transient(#[source] anyhow::Error),
}

/// Whether an acquire error exhausted the transient retry budget.
///
/// This classifies the error only. The caller retains the broker session and
/// acquisition state after any acquire error, including local preflight
/// failures; this flag does not request session teardown.
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
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build run-service HTTP client")?;
        Ok(Self {
            http,
            bearer_token: bearer_token.into(),
            #[cfg(any(test, feature = "test-support"))]
            #[allow(
                clippy::unwrap_used,
                clippy::expect_used,
                clippy::panic,
                clippy::unreachable,
                clippy::todo,
                clippy::unimplemented,
                reason = "tests may panic"
            )]
            acquire_retry_delay_override: None,
            #[cfg(test)]
            #[allow(
                clippy::unwrap_used,
                clippy::expect_used,
                clippy::panic,
                clippy::unreachable,
                clippy::todo,
                clippy::unimplemented,
                reason = "tests may panic"
            )]
            complete_retry_delay_override: None,
        })
    }

    /// Override acquire retry delay in unit and `test-support` builds.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    pub fn with_acquire_retry_delay_for_test(mut self, delay: Duration) -> Self {
        self.acquire_retry_delay_override = Some(delay);
        self
    }

    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    pub(crate) fn with_complete_retry_delay_for_test(mut self, delay: Duration) -> Self {
        self.complete_retry_delay_override = Some(delay);
        self
    }

    pub async fn acquire_job(
        &self,
        run_service_url: &str,
        job_message_id: &str,
        runner_os: &str,
        billing_owner_id: Option<&str>,
    ) -> Result<AcquireJobOutcome> {
        let mut attempt = 1;
        loop {
            let outcome = match run_service_acquire_job_url(run_service_url).and_then(|url| {
                serde_json::to_string(&AcquireJobRequest {
                    job_message_id,
                    runner_os,
                    billing_owner_id,
                })
                .context("serialize acquire job request")
                .map(|body| (url, body))
            }) {
                Ok((url, body)) => {
                    github_json_http_response(
                        "POST",
                        url.as_str(),
                        &self.bearer_token,
                        Some(body),
                        30,
                    )
                    .await
                }
                Err(error) => Err(error),
            };

            let retry_error = match outcome {
                Ok(response) => {
                    let status = response.status;
                    let text = response.body;
                    let content_length = response
                        .headers
                        .get(reqwest::header::CONTENT_LENGTH)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse().ok());
                    let content_type = response
                        .headers
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok());
                    match classify_acquire_job_response(status, &text) {
                        AcquireJobResponseClass::Skipped(reason) => {
                            let status_code = StatusCode::from_u16(status)
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                            return Ok(AcquireJobOutcome::Skipped {
                                status: status_code,
                                request_id: None,
                                reason,
                                category: classify_acquire_skipped(&text),
                                body: text,
                            });
                        }
                        AcquireJobResponseClass::RetryableFailure => {
                            Some(github_api_error_categorized(
                                "acquire run-service job",
                                status,
                                text,
                                // RunServer retries every acquire exception
                                // except typed 404/409/422. Keep generic
                                // non-2xx responses on that retry path.
                                BrokerErrorCategory::Transient,
                            ))
                        }
                        AcquireJobResponseClass::Success => {
                            match decode_acquire_job_success_body(
                                status,
                                content_length,
                                content_type,
                                &text,
                            ) {
                                Ok(value) => {
                                    return Ok(AcquireJobOutcome::Acquired(Box::new(value)))
                                }
                                Err(error) => Some(github_api_error_categorized(
                                    "acquire run-service job",
                                    status,
                                    format!(
                                        "parse acquire AgentJobRequestMessage response: {error}"
                                    ),
                                    BrokerErrorCategory::Transient,
                                )),
                            }
                        }
                    }
                }
                Err(error) => Some(error.context("acquire run-service job request")),
            };

            // Proof: every `Ok` success path returns above; reaching here
            // requires the `Err` arm, which always yields `Some`.
            #[allow(clippy::unreachable, reason = "only the Err arm falls through")]
            let Some(error) = retry_error
            else {
                unreachable!("successful acquire returns before retry handling");
            };
            // Match RunServer's retry boundary: typed 404/409/422 already
            // returned as `Skipped`; every other exception retries within
            // the local five-attempt bound.
            if attempt >= RUN_SERVICE_ACQUIRE_MAX_ATTEMPTS {
                return Err(AcquireJobError::Transient(error).into());
            }

            let delay = self.acquire_retry_delay(attempt);
            eprintln!(
                "acquire run-service job attempt {attempt}/{RUN_SERVICE_ACQUIRE_MAX_ATTEMPTS} failed ({error:#}); retrying in {}ms",
                delay.as_millis()
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    fn acquire_retry_delay(&self, _attempt: u32) -> Duration {
        #[cfg(any(test, feature = "test-support"))]
        #[allow(
            clippy::unwrap_used,
            clippy::expect_used,
            clippy::panic,
            clippy::unreachable,
            clippy::todo,
            clippy::unimplemented,
            reason = "tests may panic"
        )]
        if let Some(delay) = self.acquire_retry_delay_override {
            return delay;
        }

        let span = RUN_SERVICE_ACQUIRE_RETRY_MAX_MS - RUN_SERVICE_ACQUIRE_RETRY_MIN_MS;
        Duration::from_millis(RUN_SERVICE_ACQUIRE_RETRY_MIN_MS + random_u64_below(span))
    }

    fn complete_retry_delay(&self, attempt: u32) -> Duration {
        #[cfg(test)]
        #[allow(
            clippy::unwrap_used,
            clippy::expect_used,
            clippy::panic,
            clippy::unreachable,
            clippy::todo,
            clippy::unimplemented,
            reason = "tests may panic"
        )]
        if let Some(delay) = self.complete_retry_delay_override {
            return delay;
        }

        Duration::from_secs(5u64.saturating_mul(1 << (attempt - 1)).min(60))
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
        let payload = serde_json::to_vec(&completion).context("serialize complete job request")?;
        self.complete_job_payload(run_service_url, payload).await
    }

    /// Report the job result and return whether the remote service observed it
    /// as already terminal. The acknowledgement is not journaled here because
    /// this protocol client does not own the control journal.
    pub async fn complete_job_with_acknowledgement(
        &self,
        run_service_url: &str,
        completion: RunServiceCompleteJob,
    ) -> Result<CompletionAcknowledgement> {
        let payload = serde_json::to_vec(&completion).context("serialize complete job request")?;
        self.complete_job_payload_with_acknowledgement(run_service_url, payload)
            .await
    }

    /// Report a previously serialized job result without changing its JSON.
    /// Used by crash recovery to preserve the exact durable outbox payload.
    pub async fn complete_job_payload(
        &self,
        run_service_url: &str,
        payload: Vec<u8>,
    ) -> Result<()> {
        self.complete_job_payload_with_acknowledgement(run_service_url, payload)
            .await
            .map(|_| ())
    }

    /// Report previously serialized completion bytes and preserve the remote
    /// terminal disposition for the journal-owning caller.
    pub async fn complete_job_payload_with_acknowledgement(
        &self,
        run_service_url: &str,
        payload: Vec<u8>,
    ) -> Result<CompletionAcknowledgement> {
        const MAX_ATTEMPTS: u32 = 6;
        let url = run_service_complete_job_url(run_service_url)?;
        let body = String::from_utf8(payload).context("complete job payload is not UTF-8")?;
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
            let category: Option<BrokerErrorCategory> = match &outcome {
                Ok((status, body)) => match classify_completion_response(*status, body) {
                    CompletionResponseClass::Accepted => {
                        return Ok(CompletionAcknowledgement::Accepted);
                    }
                    CompletionResponseClass::RemoteObservedTerminal => {
                        return Ok(CompletionAcknowledgement::RemoteObservedTerminal);
                    }
                    CompletionResponseClass::RetryableFailure => {
                        Some(BrokerErrorCategory::Transient)
                    }
                    CompletionResponseClass::PermanentFailure => {
                        Some(BrokerErrorCategory::Terminal)
                    }
                },
                // Status-less transport failure: unclassified, fail open to
                // retry — not knowing the remote's answer is the case where
                // retrying is the only correct move.
                Err(_) => None,
            };
            // Category-driven policy: only the boundary's `Terminal` verdict
            // fails fast; `Transient` and unclassified transport failures
            // sleep and retry inside the unchanged 6-attempt budget.
            let terminal = matches!(category, Some(BrokerErrorCategory::Terminal));
            if attempt >= MAX_ATTEMPTS || terminal {
                // Every classified failure leaves the boundary carrying its
                // category, so the journal's abandon policy reads the
                // boundary verdict instead of re-deriving it. Status-less
                // transport failures stay unclassified: the policy fails
                // open to retry for those.
                return match outcome {
                    Ok((status, text)) => {
                        // Proof: every `Ok` failure arm classifies above;
                        // reaching here with `None` requires the `Err` arm.
                        #[allow(clippy::unreachable, reason = "only the Err arm is unclassified")]
                        let Some(category) = category
                        else {
                            unreachable!("classified completion failure always carries a category");
                        };
                        Err(github_api_error_categorized(
                            "complete run-service job",
                            status,
                            text,
                            category,
                        ))
                    }
                    Err(error) => Err(error).context("complete run-service job request"),
                };
            }
            let delay = self.complete_retry_delay(attempt);
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

impl DistributedTaskClient {
    pub fn new(server_url: &str, bearer_token: impl Into<String>) -> Result<Self> {
        validate_known_service_url(
            server_url,
            "SystemVssConnection",
            &["pipelines.actions.githubusercontent.com"],
        )?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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
    let validated = validate_known_service_url(
        run_service_url,
        "run-service",
        &["run.actions.githubusercontent.com"],
    )?;
    slash_url(validated.as_str())?
        .join("acquirejob")
        .context("build run-service acquire job URL")
}

fn run_service_renew_job_url(run_service_url: &str) -> Result<Url> {
    let validated = validate_known_service_url(
        run_service_url,
        "run-service",
        &["run.actions.githubusercontent.com"],
    )?;
    slash_url(validated.as_str())?
        .join("renewjob")
        .context("build run-service renew job URL")
}

fn run_service_complete_job_url(run_service_url: &str) -> Result<Url> {
    let validated = validate_known_service_url(
        run_service_url,
        "run-service",
        &["run.actions.githubusercontent.com"],
    )?;
    slash_url(validated.as_str())?
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

    if status.is_success() {
        let content_length = response.content_length();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = response.text().await.map_err(|error| {
            github_api_error_categorized(
                "acquire run-service job",
                status.as_u16(),
                format!(
                    "request_id={}, read acquire AgentJobRequestMessage response: {error}",
                    request_id.as_deref().unwrap_or("unknown")
                ),
                BrokerErrorCategory::Transient,
            )
        })?;
        return match decode_acquire_job_success_body(
            status.as_u16(),
            content_length,
            content_type.as_deref(),
            &body,
        ) {
            Ok(value) => Ok(AcquireJobOutcome::Acquired(Box::new(value))),
            Err(error) => Err(github_api_error_categorized(
                "acquire run-service job",
                status.as_u16(),
                format!(
                    "request_id={}, parse acquire AgentJobRequestMessage response: {error}",
                    request_id.unwrap_or_else(|| "unknown".to_string())
                ),
                BrokerErrorCategory::Transient,
            )),
        };
    }

    let body = response.text().await.unwrap_or_default();
    if let AcquireJobResponseClass::Skipped(reason) =
        classify_acquire_job_response(status.as_u16(), &body)
    {
        return Ok(AcquireJobOutcome::Skipped {
            status,
            request_id,
            reason,
            category: classify_acquire_skipped(&body),
            body,
        });
    }

    Err(github_api_error_categorized(
        "acquire run-service job",
        status.as_u16(),
        format!(
            "request_id={}, body={}",
            request_id.unwrap_or_else(|| "unknown".to_string()),
            body
        ),
        BrokerErrorCategory::Transient,
    ))
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

#[derive(Clone, Serialize, Deserialize)]
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

impl fmt::Debug for TaskAgentSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskAgentSession")
            .field("session_id", &self.session_id)
            .field("owner_name", &self.owner_name)
            .field("agent", &self.agent)
            .field("use_fips_encryption", &self.use_fips_encryption)
            .field("encryption_key", &self.encryption_key)
            .finish()
    }
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

#[derive(Clone, Serialize, Deserialize)]
pub struct TaskAgentSessionKey {
    #[serde(rename = "encrypted")]
    pub encrypted: bool,
    #[serde(rename = "value")]
    pub value: String,
}

impl fmt::Debug for TaskAgentSessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskAgentSessionKey")
            .field("encrypted", &self.encrypted)
            .field("value", &"<redacted>")
            .finish()
    }
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

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub session_id: String,
    pub encryption_key: Option<EncryptionKey>,
}

impl fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentSession")
            .field("session_id", &self.session_id)
            .field("encryption_key", &self.encryption_key)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EncryptionKey {
    pub encrypted: bool,
    pub value_base64: String,
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptionKey")
            .field("encrypted", &self.encrypted)
            .field("value_base64", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
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

impl fmt::Debug for TaskAgentMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskAgentMessage")
            .field("message_id", &self.message_id)
            .field("message_type", &self.message_type)
            .field("body", &"<redacted>")
            .field("iv_base64", &self.iv_base64)
            .finish()
    }
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

#[derive(Clone, Serialize)]
pub struct RunServiceVariableValue {
    #[serde(rename = "value")]
    pub value: String,
    #[serde(rename = "isSecret")]
    pub is_secret: bool,
}

impl fmt::Debug for RunServiceVariableValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = if self.is_secret {
            "<redacted>"
        } else {
            self.value.as_str()
        };
        f.debug_struct("RunServiceVariableValue")
            .field("value", &value)
            .field("is_secret", &self.is_secret)
            .finish()
    }
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

#[derive(Clone, Serialize, Deserialize)]
pub struct JobOutputValue {
    #[serde(default, rename = "value", skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, rename = "isSecret")]
    pub is_secret: bool,
}

impl fmt::Debug for JobOutputValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobOutputValue")
            .field(
                "value",
                &if self.is_secret {
                    self.value.as_ref().map(|_| "<redacted>")
                } else {
                    self.value.as_deref()
                },
            )
            .field("is_secret", &self.is_secret)
            .finish()
    }
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

impl TaskResult {
    /// Parse a wire `result` string into the protocol enum.
    ///
    /// `actions/runner` serializes the C# enum PascalCase while Velnor
    /// canonicalizes lowercase; both spellings arrive on the wire, so
    /// this mirrors the serde `rename` + `alias` pairs above exactly.
    /// Unknown spellings return `None` and never guess.
    #[must_use]
    pub fn parse_wire(raw: &str) -> Option<Self> {
        match raw {
            "succeeded" | "Succeeded" => Some(Self::Succeeded),
            "failed" | "Failed" => Some(Self::Failed),
            "canceled" | "Canceled" => Some(Self::Canceled),
            "skipped" | "Skipped" => Some(Self::Skipped),
            "abandoned" | "Abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
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
        validate_known_service_url(
            &url,
            "FeedStreamUrl",
            &[
                "pipelines.actions.githubusercontent.com",
                "results-receiver.actions.githubusercontent.com",
            ],
        )
        .ok()?;
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
        validate_known_service_url(
            &self.url,
            "FeedStreamUrl",
            &[
                "pipelines.actions.githubusercontent.com",
                "results-receiver.actions.githubusercontent.com",
            ],
        )?;
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

/// Per-request bound for the Results Service calls issued through
/// [`TwirpResultsClient`]'s own HTTP client — the Azure blob PUTs and the
/// step-summary Twirp calls. (The step/job-log Twirp calls already carry
/// their own 30s bound via `github_json_request`.) Without this, a stalled
/// blob endpoint wedges terminal completion, which awaits the uploads.
const TWIRP_RESULTS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Client for the GitHub Actions Results Service Twirp API.
pub struct TwirpResultsClient {
    results_service_url: String,
    token: String,
    http: Client,
}

impl TwirpResultsClient {
    pub fn new(results_service_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        Self::new_with_timeout(results_service_url, token, TWIRP_RESULTS_REQUEST_TIMEOUT)
    }

    pub(crate) fn new_with_timeout(
        results_service_url: impl Into<String>,
        token: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let results_service_url = results_service_url.into();
        let results_service_url = validate_known_service_url(
            &results_service_url,
            "ResultsServiceUrl",
            &["results-receiver.actions.githubusercontent.com"],
        )?;
        Ok(Self {
            results_service_url: results_service_url
                .to_string()
                .trim_end_matches('/')
                .to_string(),
            token: token.into(),
            http: Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
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
        // list went incomplete in the UI).
        // Retry a couple times so a transient blip never loses a step record.
        let body_json = serde_json::to_string(&body).context("serialize WorkflowStepsUpdate")?;
        let mut last_err = String::new();
        for attempt in 0..3 {
            match github_json_request("POST", &url, &self.token, Some(body_json.clone()), 30).await
            {
                Ok((status, _)) if (200..300).contains(&status) => return Ok(()),
                Ok((status, resp)) => {
                    last_err = format!("status={status}, body={}", sanitize_response_body(&resp))
                }
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
            bail!(
                "GetJobLogsSignedBlobURL failed: status={status}, body={}",
                sanitize_response_body(&body)
            );
        }
        let resp: GetUrlResp =
            serde_json::from_str(&body).context("GetJobLogsSignedBlobURL parse")?;
        let logs_url = resp
            .logs_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GetJobLogsSignedBlobURL returned empty URL"))
            .and_then(|url| validate_signed_blob_url(&url, "job log"))?;

        let put_resp = self
            .http
            .put(logs_url)
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
            bail!(
                "job log PUT failed: status={put_status}, body={}",
                sanitize_response_body(&body)
            );
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
            bail!(
                "CreateJobLogsMetadata failed: status={meta_status}, body={}",
                sanitize_response_body(&meta_body_resp)
            );
        }
        let meta_resp: MetaResp =
            serde_json::from_str(&meta_body_resp).context("CreateJobLogsMetadata parse")?;
        if !meta_resp.ok {
            bail!(
                "CreateJobLogsMetadata returned ok=false: body={}",
                sanitize_response_body(&meta_body_resp)
            );
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
            bail!(
                "GetStepLogsSignedBlobURL failed: status={status}, body={}",
                sanitize_response_body(&body)
            );
        }
        let resp: GetUrlResp =
            serde_json::from_str(&body).context("GetStepLogsSignedBlobURL parse")?;
        let logs_url = resp
            .logs_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GetStepLogsSignedBlobURL returned empty URL"))
            .and_then(|url| validate_signed_blob_url(&url, "step log"))?;

        // 2. PUT log content to Azure blob (single block; reqwest — not GitHub infra).
        let put_resp = self
            .http
            .put(logs_url)
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
            bail!(
                "step log PUT failed: status={put_status}, body={}",
                sanitize_response_body(&body)
            );
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
            bail!(
                "CreateStepLogsMetadata failed: status={meta_status}, body={}",
                sanitize_response_body(&meta_body_resp)
            );
        }
        let meta_resp: MetaResp =
            serde_json::from_str(&meta_body_resp).context("CreateStepLogsMetadata parse")?;
        if !meta_resp.ok {
            bail!(
                "CreateStepLogsMetadata returned ok=false: body={}",
                sanitize_response_body(&meta_body_resp)
            );
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
            .ok_or_else(|| anyhow::anyhow!("GetStepSummarySignedBlobURL returned empty URL"))
            .and_then(|url| validate_signed_blob_url(&url, "step summary"))?;

        // 2. Upload summary content.
        let content_bytes = content.as_bytes().to_vec();
        let content_len = content_bytes.len();
        let put_resp = self
            .http
            .put(blob_url)
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
            bail!(
                "step summary PUT failed: status={put_status}, body={}",
                sanitize_response_body(&body)
            );
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
            bail!(
                "CreateStepSummaryMetadata failed: status={meta_status}, body={}",
                sanitize_response_body(&body)
            );
        }
        Ok(())
    }
}

fn write_artifact_zip_temp_file(
    path: std::path::PathBuf,
    files: &[(String, Vec<u8>)],
    store_uncompressed: bool,
) -> Result<(ArtifactTempFile, u64, String)> {
    use std::io::Write;

    ensure_upload_file_count(files.len())?;
    validate_upload_archive_paths(files.iter().map(|(path, _)| path.as_str()))?;
    let mut source_total = 0_u64;
    for (_, content) in files {
        source_total = checked_upload_source_add(source_total, content.len() as u64)?;
    }

    let (temp, file) = open_artifact_temp_file(path).context("create artifact zip temp file")?;
    let mut zip = zip::ZipWriter::new(BoundedZipWriter::new(
        file,
        RESULTS_ARTIFACT_MAX_UPLOAD_ZIP_BYTES,
    ));
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
    finish_artifact_zip(zip, temp)
}

fn write_artifact_zip_from_files_temp_file(
    path: std::path::PathBuf,
    files: &[ArtifactUploadFile],
    store_uncompressed: bool,
) -> Result<(ArtifactTempFile, u64, String)> {
    use std::io::{Read, Seek, SeekFrom};

    ensure_upload_file_count(files.len())?;
    validate_upload_archive_paths(files.iter().map(|file| file.archive_path.as_str()))?;
    let (temp, file) = open_artifact_temp_file(path).context("create artifact zip temp file")?;
    let mut zip = zip::ZipWriter::new(BoundedZipWriter::new(
        file,
        RESULTS_ARTIFACT_MAX_UPLOAD_ZIP_BYTES,
    ));
    let method = if store_uncompressed {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    let options = zip::write::FileOptions::<()>::default().compression_method(method);
    let mut source_total = 0_u64;
    for source in files {
        let mut input = source.source.open(&source.source_path)?;
        let metadata = input.metadata().with_context(|| {
            format!(
                "stat opened artifact source {}",
                source.source_path.display()
            )
        })?;
        if !metadata.is_file() {
            bail!(
                "artifact source {} is not a regular file",
                source.source_path.display()
            );
        }
        input
            .seek(SeekFrom::Start(0))
            .with_context(|| format!("rewind artifact source {}", source.source_path.display()))?;
        checked_upload_source_add(source_total, metadata.len())?;
        zip.start_file(&source.archive_path, options)
            .context("zip start_file")?;
        let remaining = RESULTS_ARTIFACT_MAX_UPLOAD_SOURCE_BYTES - source_total;
        let mut limited = (&mut input).take(remaining.saturating_add(1));
        let copied = std::io::copy(&mut limited, &mut zip)
            .with_context(|| format!("read artifact source {}", source.source_path.display()))?;
        if copied > remaining {
            bail!(
                "artifact upload source bytes exceed the {}-byte limit; split the artifact into smaller uploads",
                RESULTS_ARTIFACT_MAX_UPLOAD_SOURCE_BYTES
            );
        }
        source_total += copied;
    }
    finish_artifact_zip(zip, temp)
}

fn finish_artifact_zip(
    zip: zip::ZipWriter<BoundedZipWriter<std::fs::File>>,
    temp: ArtifactTempFile,
) -> Result<(ArtifactTempFile, u64, String)> {
    use std::io::{Seek, Write};

    let mut writer = zip.finish().context("zip finish")?;
    writer.flush().context("flush artifact zip temp file")?;
    let (mut file, zip_size) = writer.into_parts();
    file.set_len(zip_size)
        .context("truncate artifact zip temp file")?;
    file.seek(std::io::SeekFrom::Start(0))
        .context("rewind artifact zip temp file")?;
    let mut temp = temp;
    temp.file = Some(file);
    let zip_hash = hash_artifact_file(
        temp.file
            .as_mut()
            .context("retain artifact zip temp file")?,
    )?;
    Ok((temp, zip_size, zip_hash))
}

struct BoundedZipWriter<W> {
    inner: W,
    position: u64,
    written: u64,
    limit: u64,
}

impl<W> BoundedZipWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            position: 0,
            written: 0,
            limit,
        }
    }

    fn into_parts(self) -> (W, u64) {
        (self.inner, self.written)
    }
}

impl<W: std::io::Write> std::io::Write for BoundedZipWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let end = self
            .position
            .checked_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("artifact ZIP byte count overflowed"))?;
        if end > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                format!("artifact ZIP payload exceeds the {}-byte limit", self.limit),
            ));
        }
        let written = self.inner.write(buffer)?;
        self.position = self
            .position
            .checked_add(u64::try_from(written).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("artifact ZIP byte count overflowed"))?;
        self.written = self.written.max(self.position);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: std::io::Seek> std::io::Seek for BoundedZipWriter<W> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        let position = self.inner.seek(position)?;
        self.position = position;
        Ok(position)
    }
}

fn ensure_upload_file_count(count: usize) -> Result<()> {
    if count > RESULTS_ARTIFACT_MAX_UPLOAD_FILES {
        bail!(
            "artifact contains {count} files, exceeding the {}-file upload limit; split it into artifacts",
            RESULTS_ARTIFACT_MAX_UPLOAD_FILES
        );
    }
    Ok(())
}

fn validate_upload_archive_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut path_bytes = 0_u64;
    let mut central_directory_bytes = 0_u64;
    for path in paths {
        let relative = std::path::Path::new(path);
        let mut depth = 0_usize;
        for component in relative.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                bail!("artifact archive path is not normalized: {path}");
            }
            depth = depth
                .checked_add(1)
                .context("artifact archive path depth overflowed")?;
        }
        if depth == 0 {
            bail!("artifact archive path is empty");
        }
        if depth > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH {
            bail!(
                "artifact archive path {path} has depth {depth}, exceeding the {}-component limit",
                RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH
            );
        }
        if !seen.insert(path) {
            bail!("artifact archive contains duplicate path: {path}");
        }
        let path_len = u64::try_from(path.len()).unwrap_or(u64::MAX);
        path_bytes = path_bytes
            .checked_add(path_len)
            .context("artifact upload path byte count overflowed")?;
        if path_bytes > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES {
            bail!(
                "artifact upload paths use {path_bytes} bytes, exceeding the {}-byte limit",
                RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES
            );
        }
        central_directory_bytes = central_directory_bytes
            .checked_add(46_u64)
            .and_then(|value| value.checked_add(path_len))
            .and_then(|value| value.checked_add(64_u64))
            .context("artifact upload central-directory size overflowed")?;
        if central_directory_bytes > RESULTS_ARTIFACT_MAX_ZIP_CENTRAL_DIRECTORY_BYTES {
            bail!(
                "artifact upload central directory exceeds the {}-byte limit",
                RESULTS_ARTIFACT_MAX_ZIP_CENTRAL_DIRECTORY_BYTES
            );
        }
    }
    Ok(())
}

fn checked_upload_source_add(current: u64, additional: u64) -> Result<u64> {
    let total = current
        .checked_add(additional)
        .context("artifact upload source byte count overflowed")?;
    if total > RESULTS_ARTIFACT_MAX_UPLOAD_SOURCE_BYTES {
        bail!(
            "artifact upload source bytes exceed the {}-byte limit; split the artifact into smaller uploads",
            RESULTS_ARTIFACT_MAX_UPLOAD_SOURCE_BYTES
        );
    }
    Ok(total)
}

fn hash_artifact_file(file: &mut std::fs::File) -> Result<String> {
    use std::io::{Read, Seek};

    file.seek(std::io::SeekFrom::Start(0))
        .context("rewind artifact zip for hashing")?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .context("read artifact zip for hashing")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
        // Protobuf JSON maps google.protobuf.StringValue to a JSON string,
        // not the wrapper's object-shaped Rust representation.
        "mime_type": "application/zip",
        "version": 7
    });
    if let Some(days) = retention_days {
        let expires_at = (now + time::Duration::days(i64::from(days)))
            .format(&time::format_description::well_known::Rfc3339)
            .context("format artifact expiration")?;
        request["expires_at"] = serde_json::Value::String(expires_at);
    }
    Ok(request)
}

#[derive(Debug)]
struct ArtifactTempFile {
    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
impl ArtifactTempFile {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ArtifactTempFile {
    fn drop(&mut self) {
        #[cfg(test)]
        #[allow(
            clippy::unwrap_used,
            clippy::expect_used,
            clippy::panic,
            clippy::unreachable,
            clippy::todo,
            clippy::unimplemented,
            reason = "tests may panic"
        )]
        let _ = std::fs::remove_file(&self.path);
    }
}

fn write_artifact_temp_file(
    path: std::path::PathBuf,
    content: &[u8],
) -> std::io::Result<ArtifactTempFile> {
    use std::io::Write;

    let (temp, mut file) = open_artifact_temp_file(path)?;
    file.write_all(content)?;
    Ok(temp)
}

fn open_artifact_temp_file(
    path: std::path::PathBuf,
) -> std::io::Result<(ArtifactTempFile, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;

    // Acquire ownership atomically before constructing the cleanup guard.
    // A failed create_new must never allow Drop to remove another owner's file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    #[cfg(not(test))]
    std::fs::remove_file(&path)?;
    Ok((
        ArtifactTempFile {
            #[cfg(test)]
            #[allow(
                clippy::unwrap_used,
                clippy::expect_used,
                clippy::panic,
                clippy::unreachable,
                clippy::todo,
                clippy::unimplemented,
                reason = "tests may panic"
            )]
            path,
            file: None,
        },
        file,
    ))
}

fn results_service_post(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    body: &str,
    operation: &str,
) -> Result<String> {
    let response = client
        .post(url)
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(30))
        .body(body.to_owned())
        .send()
        .map_err(|error| {
            anyhow::anyhow!(
                "send Results Service {operation}: {}",
                redacted_reqwest_error(&error)
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let mut response = response;
        let response_body = read_bounded_response_preview(&mut response);
        bail!(
            "Results Service {operation}: status={status}, body={}",
            response_body
        );
    }
    if let Some(content_length) = response.content_length() {
        ensure_artifact_size_limit(
            "Results Service",
            "control response",
            content_length,
            RESULTS_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES,
            "reduce the response size",
        )?;
    }
    let mut response = response;
    read_bounded_response_body(
        &mut response,
        operation,
        RESULTS_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES,
    )
}

fn artifact_transfer_timeout(bytes: u64) -> Duration {
    let transfer_seconds = bytes.saturating_add(ARTIFACT_TRANSFER_MIN_BYTES_PER_SECOND - 1)
        / ARTIFACT_TRANSFER_MIN_BYTES_PER_SECOND;
    Duration::from_secs(
        transfer_seconds
            .saturating_add(ARTIFACT_TRANSFER_GRACE_SECONDS)
            .min(ARTIFACT_TRANSFER_MAX_SECONDS),
    )
}

fn read_bounded_response_body(
    reader: &mut impl std::io::Read,
    operation: &str,
    limit: u64,
) -> Result<String> {
    use std::io::Read;

    let mut bytes = Vec::new();
    let mut limited = reader.take(limit.saturating_add(1));
    limited
        .read_to_end(&mut bytes)
        .with_context(|| format!("read Results Service {operation} response"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        bail!(
            "Results Service {operation} response exceeds the {limit}-byte limit; reduce the response size"
        );
    }
    String::from_utf8(bytes)
        .with_context(|| format!("Results Service {operation} response was not UTF-8"))
}

/// Upload artifact files to GitHub's Results Service (artifact v4 format).
///
/// Uses synchronous `reqwest::blocking` — safe to call from `tokio::task::spawn_blocking`
/// threads (the Velnor job executor context).
///
/// Flow: CreateArtifact → PUT zip to signed URL → FinalizeArtifact
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactUploadOptions {
    pub(crate) store_uncompressed: bool,
    pub(crate) retention_days: Option<u8>,
    pub(crate) overwrite: bool,
}

#[derive(Debug, Deserialize)]
struct CreateArtifactResponse {
    ok: bool,
    #[serde(alias = "signedUploadUrl")]
    signed_upload_url: String,
}

#[derive(Debug, Deserialize)]
struct FinalizeArtifactResponse {
    ok: bool,
    #[serde(alias = "artifactId")]
    artifact_id: WireU64,
}

#[derive(Debug, Serialize)]
struct FinalizeArtifactRequest<'a> {
    #[serde(rename = "workflow_run_backend_id")]
    workflow_run_backend_id: &'a str,
    #[serde(rename = "workflow_job_run_backend_id")]
    workflow_job_run_backend_id: &'a str,
    name: &'a str,
    size: String,
    // google.protobuf.StringValue uses a JSON string in protobuf JSON. Keep
    // this field typed as String so the wrapper-object shape cannot reappear.
    hash: String,
}

#[derive(Debug, Deserialize)]
struct DeleteArtifactResponse {
    ok: Option<bool>,
    #[serde(alias = "artifactId")]
    artifact_id: WireU64,
}

#[derive(Debug, Deserialize)]
struct GetSignedArtifactUrlResponse {
    #[serde(alias = "signedUrl")]
    signed_url: String,
}

#[derive(Debug, Deserialize)]
struct ListArtifactsResponse {
    artifacts: Option<Vec<ResultsArtifactDescriptorWire>>,
}

#[derive(Debug, Deserialize)]
struct ResultsArtifactDescriptorWire {
    workflow_run_backend_id: String,
    workflow_job_run_backend_id: String,
    #[serde(alias = "databaseId")]
    database_id: WireU64,
    name: String,
    size: WireU64,
    digest: WireStringValue,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireU64 {
    Number(u64),
    String(String),
}

impl WireU64 {
    fn parse(&self, field: &str) -> Result<u64> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(value) => value
                .parse()
                .with_context(|| format!("{field} is not an unsigned integer")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireStringValue {
    String(String),
    Object { value: String },
}

impl WireStringValue {
    fn as_str(&self) -> &str {
        match self {
            Self::String(value) | Self::Object { value } => value,
        }
    }
}

#[derive(Debug, Clone)]
struct ValidatedResultsArtifactDescriptor {
    workflow_run_backend_id: String,
    workflow_job_run_backend_id: String,
    database_id: u64,
    name: String,
    size: u64,
    digest: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FinalizedArtifact {
    pub(crate) id: String,
    pub(crate) digest: String,
}

fn validate_results_artifact_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().any(|character| {
            matches!(
                character,
                '"' | ':' | '<' | '>' | '|' | '*' | '?' | '\r' | '\n' | '\\' | '/' | '\0'
            )
        })
    {
        bail!("Results Service artifact name is empty or contains unsafe path characters");
    }
    Ok(())
}

fn validate_sha256_digest(digest: &str, field: &str) -> Result<String> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        bail!("{field} must use the sha256:<64 hex digits> format");
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must use the sha256:<64 hex digits> format");
    }
    Ok(digest.to_ascii_lowercase())
}

impl ResultsArtifactDescriptorWire {
    /// Structural validation only. The backend IDs of a listed row are
    /// consumed as-is: actions/runner and actions/toolkit map the whole
    /// ListArtifacts response without comparing `workflow_run_backend_id`
    /// against the caller's token scope
    /// (`actions/toolkit/packages/artifact/src/internal/find/list-artifacts.ts`),
    /// and every follow-up request must echo the row's own IDs
    /// (GetSignedArtifactURL, DeleteArtifact). A row whose IDs differ from the
    /// token scope is still this run's artifact — the run backend can re-issue
    /// a job with a new scope after a redelivery, while rows written by
    /// earlier attempts keep their original IDs. Selection is by name and
    /// pattern, never by ID equality.
    fn validate(self) -> Result<ValidatedResultsArtifactDescriptor> {
        validate_backend_id(&self.workflow_run_backend_id, "workflow_run_backend_id")?;
        validate_backend_id(
            &self.workflow_job_run_backend_id,
            "workflow_job_run_backend_id",
        )?;
        validate_results_artifact_name(&self.name)?;
        let database_id = self.database_id.parse("database_id")?;
        if database_id == 0 {
            bail!(
                "Results Service artifact '{}' has an invalid database_id",
                self.name
            );
        }
        let size = self.size.parse("size")?;
        let digest = validate_sha256_digest(self.digest.as_str(), "artifact digest")?;
        Ok(ValidatedResultsArtifactDescriptor {
            workflow_run_backend_id: self.workflow_run_backend_id,
            workflow_job_run_backend_id: self.workflow_job_run_backend_id,
            database_id,
            name: self.name,
            size,
            digest,
        })
    }
}

fn validate_backend_id(value: &str, field: &str) -> Result<()> {
    const MAX_BACKEND_ID_BYTES: usize = 256;

    if value.is_empty() || value.len() > MAX_BACKEND_ID_BYTES || value.chars().any(char::is_control)
    {
        bail!("Results Service {field} is empty, oversized, or contains control characters");
    }
    Ok(())
}

fn digest_matches(expected: &str, actual_hex: &str) -> bool {
    expected
        .strip_prefix("sha256:")
        .is_some_and(|expected| expected.eq_ignore_ascii_case(actual_hex))
}

/// A file-backed artifact input. The upload path reads each source in bounded
/// chunks, so artifact contents do not need to be materialized in memory.
#[derive(Debug)]
pub(crate) struct ArtifactUploadFile {
    pub(crate) archive_path: String,
    pub(crate) source: ArtifactUploadSource,
    pub(crate) source_path: std::path::PathBuf,
}

#[derive(Debug)]
pub(crate) enum ArtifactUploadSource {
    Opened(std::fs::File),
    Relative {
        root: Arc<crate::fs_copy::NoFollowDir>,
        relative: std::path::PathBuf,
    },
}

impl ArtifactUploadSource {
    fn open(&self, display_path: &std::path::Path) -> Result<std::fs::File> {
        match self {
            Self::Opened(file) => file
                .try_clone()
                .with_context(|| format!("duplicate artifact source {}", display_path.display())),
            Self::Relative { root, relative } => {
                let source = root
                    .open_source(relative)
                    .with_context(|| format!("open artifact source {}", display_path.display()))?;
                match source {
                    Some(crate::fs_copy::NoFollowSource::File(file)) => Ok(file),
                    Some(crate::fs_copy::NoFollowSource::Directory(_)) => bail!(
                        "artifact source {} is not a regular file",
                        display_path.display()
                    ),
                    None => bail!("artifact source disappeared: {}", display_path.display()),
                }
            }
        }
    }
}

pub(crate) fn upload_artifact_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    files: &[(String, Vec<u8>)], // (archive path, content)
    options: ArtifactUploadOptions,
) -> Result<FinalizedArtifact> {
    upload_artifact_with_zip_builder(
        results_service_url,
        token,
        plan_id,
        job_id,
        name,
        options,
        |zip_path| write_artifact_zip_temp_file(zip_path, files, options.store_uncompressed),
    )
}

pub(crate) fn upload_artifact_files_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    files: Vec<ArtifactUploadFile>,
    options: ArtifactUploadOptions,
) -> Result<FinalizedArtifact> {
    upload_artifact_with_zip_builder(
        results_service_url,
        token,
        plan_id,
        job_id,
        name,
        options,
        move |zip_path| {
            write_artifact_zip_from_files_temp_file(zip_path, &files, options.store_uncompressed)
        },
    )
}

fn upload_artifact_with_zip_builder(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    options: ArtifactUploadOptions,
    build_zip: impl FnOnce(std::path::PathBuf) -> Result<(ArtifactTempFile, u64, String)>,
) -> Result<FinalizedArtifact> {
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    validate_results_artifact_name(name)?;
    let results_service_url = validate_known_service_url(
        results_service_url,
        "ResultsServiceUrl",
        &["results-receiver.actions.githubusercontent.com"],
    )?;
    let base = results_service_url.as_str().trim_end_matches('/');
    let tmp_dir = std::env::temp_dir();
    let zip_path = tmp_dir.join(format!("velnor-artifact-{}.zip", uuid::Uuid::new_v4()));
    let (mut zip_path, zip_size, zip_hash) = build_zip(zip_path)?;

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(RUNNER_USER_AGENT)
        .build()
        .context("build Results Service HTTP client")?;

    if options.overwrite {
        // Raw enumeration: this delete phase is the repair path for a
        // duplicated `(job, name)` pair, so it must see every row it owns.
        let existing = artifacts_owned_by_job(
            list_results_artifacts(&client, base, token, plan_id, job_id)?,
            job_id,
        )
        .into_iter()
        .filter(|artifact| artifact.name == name)
        .collect::<Vec<_>>();
        for artifact in existing {
            delete_artifact_descriptor_blocking(&client, base, token, &artifact)?;
        }
    }

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
    let create_text =
        results_service_post(&client, &create_url, token, &create_body, "CreateArtifact")
            .context("CreateArtifact request")?;
    let create_resp: CreateArtifactResponse =
        serde_json::from_str(&create_text).context("CreateArtifact parse")?;
    if !create_resp.ok {
        bail!("CreateArtifact: backend returned ok=false or absent");
    }
    let upload_url = create_resp.signed_upload_url;
    if upload_url.is_empty() {
        bail!("CreateArtifact: empty signed_upload_url");
    }
    let upload_url = validate_signed_blob_url(&upload_url, "artifact upload")?;

    // 2. PUT the prepared mode-0600 temp archive without retaining a second
    // full archive in RAM.
    use std::io::Seek;

    let mut zip_file = zip_path
        .file
        .take()
        .context("take owned artifact zip temp file")?;
    zip_file
        .seek(std::io::SeekFrom::Start(0))
        .context("rewind artifact zip temp file")?;
    let put_response = client
        .put(upload_url)
        .header("Content-Type", "application/zip")
        .header("Content-Length", zip_size)
        .header("x-ms-blob-type", "BlockBlob")
        .timeout(artifact_transfer_timeout(zip_size))
        .body(zip_file)
        .send()
        .map_err(|error| {
            anyhow::anyhow!("send artifact blob PUT: {}", redacted_reqwest_error(&error))
        })?;
    let put_status = put_response.status().as_u16();
    if !(200..300).contains(&put_status) {
        bail!("artifact blob PUT failed: status={put_status}");
    }

    // 3. FinalizeArtifact.
    let finalize_url = format!("{base}/{SERVICE}/FinalizeArtifact");
    let finalize_body = serde_json::to_string(&FinalizeArtifactRequest {
        workflow_run_backend_id: plan_id,
        workflow_job_run_backend_id: job_id,
        name,
        size: zip_size.to_string(),
        hash: format!("sha256:{zip_hash}"),
    })
    .context("serialize FinalizeArtifact")?;
    let finalize_text = results_service_post(
        &client,
        &finalize_url,
        token,
        &finalize_body,
        "FinalizeArtifact",
    )
    .context("FinalizeArtifact request")?;
    let finalize: FinalizeArtifactResponse =
        serde_json::from_str(&finalize_text).context("FinalizeArtifact parse")?;
    if !finalize.ok {
        bail!("FinalizeArtifact: backend returned ok=false or absent");
    }
    let artifact_id = finalize.artifact_id.parse("FinalizeArtifact artifact_id")?;
    if artifact_id == 0 {
        bail!("FinalizeArtifact: artifact_id must be non-zero");
    }
    Ok(FinalizedArtifact {
        id: artifact_id.to_string(),
        digest: format!("sha256:{zip_hash}"),
    })
}

fn delete_artifact_descriptor_blocking(
    client: &reqwest::blocking::Client,
    base: &str,
    token: &str,
    artifact: &ValidatedResultsArtifactDescriptor,
) -> Result<()> {
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    let body = serde_json::to_string(&serde_json::json!({
        "workflow_run_backend_id": artifact.workflow_run_backend_id,
        "workflow_job_run_backend_id": artifact.workflow_job_run_backend_id,
        "name": artifact.name,
    }))
    .context("serialize DeleteArtifact")?;
    let url = format!("{base}/{SERVICE}/DeleteArtifact");
    let text = results_service_post(client, &url, token, &body, "DeleteArtifact")?;
    let response: DeleteArtifactResponse =
        serde_json::from_str(&text).context("DeleteArtifact parse")?;
    if response.ok == Some(false) {
        bail!("DeleteArtifact: backend returned ok=false");
    }
    let response_id = response.artifact_id.parse("DeleteArtifact artifact_id")?;
    if response_id != artifact.database_id {
        bail!(
            "DeleteArtifact returned artifact_id {response_id}, expected {}",
            artifact.database_id
        );
    }
    Ok(())
}

pub(crate) fn delete_finalized_artifact_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    artifact_id: &str,
) -> Result<()> {
    let results_service_url = validate_known_service_url(
        results_service_url,
        "ResultsServiceUrl",
        &["results-receiver.actions.githubusercontent.com"],
    )?;
    let artifact_id = artifact_id
        .parse::<u64>()
        .context("Results Service artifact cleanup ID is not numeric")?;
    if artifact_id == 0 {
        bail!("Results Service artifact cleanup ID must be non-zero");
    }
    validate_results_artifact_name(name)?;
    let base = results_service_url.as_str().trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(RUNNER_USER_AGENT)
        .build()
        .context("build Results Service HTTP client")?;
    // Raw enumeration: a delete target is picked by artifact ID, so the
    // identity contract must not gate the repair path.
    let matches = artifacts_owned_by_job(
        list_results_artifacts(&client, base, token, plan_id, job_id)?,
        job_id,
    )
    .into_iter()
    .filter(|artifact| artifact.name == name && artifact.database_id == artifact_id)
    .collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!(
            "cannot safely delete Results Service artifact '{name}': expected one matching artifact ID, found {}",
            matches.len()
        );
    }
    delete_artifact_descriptor_blocking(&client, base, token, &matches[0])
}

#[derive(Debug)]
struct ArtifactDownloadStagingDirectory {
    parent: crate::fs_copy::NoFollowDestinationDir,
    name: std::ffi::OsString,
    root: Arc<crate::fs_copy::NoFollowDestinationDir>,
}

impl Drop for ArtifactDownloadStagingDirectory {
    fn drop(&mut self) {
        let _ = self.parent.remove_tree_entry(&self.name);
    }
}

fn create_artifact_download_staging_directory(
    tmp_dir: &std::path::Path,
) -> Result<Arc<ArtifactDownloadStagingDirectory>> {
    let parent = crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
        tmp_dir,
        std::path::Path::new(""),
    )
    .with_context(|| format!("open artifact download temp root {}", tmp_dir.display()))?;
    let cleanup_parent = parent.open_relative_directory(std::path::Path::new(""))?;
    let (root, name) = parent.create_unique_directory(".velnor-artifact-download")?;
    Ok(Arc::new(ArtifactDownloadStagingDirectory {
        parent: cleanup_parent,
        name,
        root: Arc::new(root),
    }))
}

#[derive(Debug)]
pub(crate) struct ResultsArtifactFile {
    pub(crate) relative_path: std::path::PathBuf,
    source: ResultsArtifactFileSource,
}

impl ResultsArtifactFile {
    pub(crate) fn file(&self) -> Result<std::fs::File> {
        match &self.source {
            ResultsArtifactFileSource::Raw(archive) => {
                use std::io::Seek;

                let mut file = archive
                    .file
                    .as_ref()
                    .context("downloaded artifact archive descriptor is unavailable")?
                    .try_clone()
                    .context("duplicate downloaded artifact archive descriptor")?;
                file.seek(std::io::SeekFrom::Start(0))
                    .context("rewind downloaded artifact")?;
                Ok(file)
            }
            ResultsArtifactFileSource::Staged { staging, relative } => staging
                .root
                .open_relative_file(relative)
                .with_context(|| format!("open staged artifact {}", relative.display())),
        }
    }
}

#[derive(Debug)]
enum ResultsArtifactFileSource {
    Raw(Arc<ArtifactTempFile>),
    Staged {
        staging: Arc<ArtifactDownloadStagingDirectory>,
        relative: std::path::PathBuf,
    },
}

#[derive(Debug)]
pub(crate) struct ResultsArtifactDownload {
    pub(crate) name: String,
    pub(crate) files: Vec<ResultsArtifactFile>,
}

fn safe_raw_artifact_path(name: &str) -> Result<std::path::PathBuf> {
    let path = std::path::Path::new(name);
    let file_name = path
        .file_name()
        .filter(|value| *value != std::ffi::OsStr::new(".") && *value != std::ffi::OsStr::new(".."))
        .context("raw artifact name has no safe file name")?;
    Ok(std::path::PathBuf::from(file_name))
}

fn artifact_response_is_zip(content_type: Option<&str>, signed_url: &str) -> bool {
    let mime_is_zip = content_type.is_some_and(|value| {
        value.split(';').next().is_some_and(|mime| {
            matches!(
                mime.trim().to_ascii_lowercase().as_str(),
                "application/zip" | "application/x-zip-compressed" | "application/zip-compressed"
            )
        })
    });
    let url_path_is_zip = Url::parse(signed_url)
        .ok()
        .and_then(|url| url.path_segments()?.next_back().map(str::to_owned))
        .is_some_and(|path| path.to_ascii_lowercase().ends_with(".zip"));
    mime_is_zip || url_path_is_zip
}

fn raw_artifact_filename(
    headers: &reqwest::header::HeaderMap,
    fallback: &str,
) -> Result<std::path::PathBuf> {
    let disposition = headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok());
    let filename = disposition.and_then(|value| {
        let parameters = value.split(';').skip(1).filter_map(|parameter| {
            let (key, value) = parameter.trim().split_once('=')?;
            Some((key.trim(), value.trim().trim_matches('"')))
        });
        let mut fallback_filename = None;
        for (key, value) in parameters {
            if key.eq_ignore_ascii_case("filename*") {
                if let Some(decoded) = decode_rfc5987_filename(value) {
                    return Some(decoded);
                }
            } else if key.eq_ignore_ascii_case("filename") && !value.is_empty() {
                fallback_filename = Some(value.to_owned());
            }
        }
        fallback_filename
    });
    safe_raw_artifact_path(
        filename
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback),
    )
}

fn decode_rfc5987_filename(value: &str) -> Option<String> {
    let (charset, encoded) = value.split_once("''")?;
    if !charset.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut chars = encoded.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let high = hex_digit(chars.next()?)?;
            let low = hex_digit(chars.next()?)?;
            bytes.push((high << 4) | low);
        } else {
            bytes.push(byte);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    (!decoded.is_empty()).then_some(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn artifact_download_status_is_ok(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::OK
}

fn ensure_artifact_size_limit(
    artifact_name: &str,
    resource: &str,
    actual: u64,
    limit: u64,
    action: &str,
) -> Result<()> {
    if actual > limit {
        bail!(
            "artifact '{artifact_name}' {resource} is {actual} bytes, exceeding the {limit}-byte limit; {action}"
        );
    }
    Ok(())
}

fn checked_artifact_size_add(
    artifact_name: &str,
    resource: &str,
    current: u64,
    additional: u64,
    limit: u64,
    action: &str,
) -> Result<u64> {
    let total = current
        .checked_add(additional)
        .with_context(|| format!("artifact '{artifact_name}' {resource} byte count overflowed"))?;
    ensure_artifact_size_limit(artifact_name, resource, total, limit, action)?;
    Ok(total)
}

fn ensure_artifact_zip_member_limit(
    artifact_name: &str,
    members: usize,
    limit: usize,
) -> Result<()> {
    if members > limit {
        bail!(
            "artifact '{artifact_name}' contains {members} ZIP members, exceeding the {limit}-member limit; split it into artifacts with fewer files"
        );
    }
    Ok(())
}

fn copy_artifact_response_bounded(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
    artifact_name: &str,
    limit: u64,
) -> Result<u64> {
    const BUFFER_BYTES: usize = 64 * 1024;

    let mut copied = 0_u64;
    let mut buffer = [0_u8; BUFFER_BYTES];
    loop {
        let remaining = limit - copied;
        let read_capacity = usize::try_from(
            remaining
                .saturating_add(1)
                .min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)),
        )
        .unwrap_or(buffer.len());
        let read = reader
            .read(&mut buffer[..read_capacity])
            .with_context(|| format!("read artifact '{artifact_name}' download response"))?;
        if read == 0 {
            return Ok(copied);
        }
        let read = u64::try_from(read).context("artifact response read size overflowed")?;
        if read > remaining {
            bail!(
                "artifact '{artifact_name}' download response exceeds the {limit}-byte limit; split the artifact into smaller uploads"
            );
        }
        writer
            .write_all(&buffer[..usize::try_from(read).unwrap_or(buffer.len())])
            .with_context(|| format!("write artifact '{artifact_name}' download temp file"))?;
        copied += read;
    }
}

fn read_bounded_response_preview(response: &mut impl std::io::Read) -> String {
    use std::io::Read;

    const PREVIEW_BYTES: u64 = 4096;

    let mut bytes = Vec::with_capacity(PREVIEW_BYTES as usize);
    let mut limited = response.take(PREVIEW_BYTES + 1);
    if limited.read_to_end(&mut bytes).is_err() {
        return "<response body unreadable>".to_string();
    }
    let truncated = bytes.len() > PREVIEW_BYTES as usize;
    bytes.truncate(PREVIEW_BYTES as usize);
    let mut preview = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        preview.push_str(" [truncated]");
    }
    sanitize_response_body(&preview)
}

fn copy_zip_entry_bounded(
    entry: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
    artifact_name: &str,
    path: &std::path::Path,
    limit: u64,
) -> Result<u64> {
    const BUFFER_BYTES: usize = 64 * 1024;

    let mut read_total = 0_u64;
    let mut buffer = [0_u8; BUFFER_BYTES];
    loop {
        let remaining = limit - read_total;
        let read_capacity = usize::try_from(
            remaining
                .saturating_add(1)
                .min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)),
        )
        .unwrap_or(buffer.len());
        let read = entry.read(&mut buffer[..read_capacity]).with_context(|| {
            format!(
                "read ZIP member '{}' from artifact '{artifact_name}'",
                path.display()
            )
        })?;
        if read == 0 {
            return Ok(read_total);
        }
        let read = u64::try_from(read).context("ZIP member read size overflowed")?;
        if read > remaining {
            bail!(
                "artifact '{artifact_name}' ZIP member '{}' exceeds the remaining {limit}-byte extraction allowance; split the artifact or reduce extracted content",
                path.display()
            );
        }
        writer
            .write_all(&buffer[..usize::try_from(read).unwrap_or(buffer.len())])
            .with_context(|| {
                format!(
                    "write ZIP member '{}' from artifact '{artifact_name}'",
                    path.display()
                )
            })?;
        read_total += read;
    }
}

#[derive(Debug, Clone, Copy)]
struct ZipCentralDirectoryMetadata {
    entries: u64,
    size: u64,
    offset: u64,
}

fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset.checked_add(2)?)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)
        .map(|value| u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes.get(offset..offset.checked_add(8)?).map(|value| {
        u64::from_le_bytes([
            value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
        ])
    })
}

fn validate_zip_central_directory(
    file: &mut std::fs::File,
    artifact_name: &str,
) -> Result<Option<ZipCentralDirectoryMetadata>> {
    use std::io::{Read, Seek, SeekFrom};

    const EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
    const ZIP64_LOCATOR_SIGNATURE: &[u8; 4] = b"PK\x06\x07";
    const ZIP64_EOCD_SIGNATURE: &[u8; 4] = b"PK\x06\x06";
    const EOCD_BYTES: u64 = 22;
    const MAX_ZIP_COMMENT_BYTES: u64 = u16::MAX as u64;
    const ZIP64_LOCATOR_BYTES: u64 = 20;

    let file_len = file
        .metadata()
        .context("stat ZIP for metadata preflight")?
        .len();
    if file_len < EOCD_BYTES {
        return Ok(None);
    }
    let tail_len = file_len.min(EOCD_BYTES + MAX_ZIP_COMMENT_BYTES);
    file.seek(SeekFrom::Start(file_len - tail_len))?;
    let mut tail = vec![0_u8; usize::try_from(tail_len).context("ZIP tail length overflowed")?];
    file.read_exact(&mut tail)?;
    let eocd_offset = tail
        .windows(EOCD_SIGNATURE.len())
        .enumerate()
        .rev()
        .find_map(|(offset, window)| {
            if window != EOCD_SIGNATURE {
                return None;
            }
            let eocd = tail.get(offset..offset.checked_add(22)?)?;
            let comment_len = u64::from(le_u16(eocd, 20)?);
            let candidate_offset = file_len - tail_len + u64::try_from(offset).ok()?;
            (candidate_offset
                .checked_add(EOCD_BYTES)?
                .checked_add(comment_len)?
                == file_len)
                .then_some(candidate_offset)
        });
    let Some(eocd_offset) = eocd_offset else {
        return Ok(None);
    };
    let eocd_index = usize::try_from(eocd_offset - (file_len - tail_len))
        .context("ZIP EOCD offset overflowed")?;
    // Proof: `EOCD_BYTES` is the constant 22, which fits in `usize` on
    // every target.
    #[allow(clippy::unwrap_used, reason = "constant 22 fits in usize")]
    let eocd = tail
        .get(eocd_index..eocd_index + usize::try_from(EOCD_BYTES).unwrap())
        .context("truncated ZIP end record")?;
    let entries16 = le_u16(eocd, 10).context("malformed ZIP entry count")?;
    let entries_on_disk16 = le_u16(eocd, 8).context("malformed ZIP disk entry count")?;
    let disk_number16 = le_u16(eocd, 4).context("malformed ZIP disk number")?;
    let disk_with_directory16 = le_u16(eocd, 6).context("malformed ZIP central-directory disk")?;
    let size32 = le_u32(eocd, 12).context("malformed ZIP central-directory size")?;
    let offset32 = le_u32(eocd, 16).context("malformed ZIP central-directory offset")?;
    if disk_number16 != 0 || disk_with_directory16 != 0 {
        bail!("artifact '{artifact_name}' is a multi-disk ZIP archive");
    }

    let (metadata, directory_end_offset) =
        if entries16 != u16::MAX && size32 != u32::MAX && offset32 != u32::MAX {
            if entries_on_disk16 != entries16 {
                bail!("artifact '{artifact_name}' has inconsistent ZIP disk entry counts");
            }
            (
                ZipCentralDirectoryMetadata {
                    entries: u64::from(entries16),
                    size: u64::from(size32),
                    offset: u64::from(offset32),
                },
                eocd_offset,
            )
        } else {
            let locator_offset = eocd_offset
                .checked_sub(ZIP64_LOCATOR_BYTES)
                .context("ZIP64 locator offset underflowed")?;
            file.seek(SeekFrom::Start(locator_offset))?;
            let mut locator = [0_u8; 20];
            file.read_exact(&mut locator)?;
            if &locator[..4] != ZIP64_LOCATOR_SIGNATURE {
                bail!("artifact '{artifact_name}' has ZIP64 markers but no ZIP64 locator");
            }
            let locator_disk = le_u32(&locator, 4).context("malformed ZIP64 locator disk")?;
            let locator_disk_count =
                le_u32(&locator, 16).context("malformed ZIP64 locator disk count")?;
            if locator_disk != 0 || locator_disk_count != 1 {
                bail!("artifact '{artifact_name}' is a multi-disk ZIP64 archive");
            }
            let record_offset = le_u64(&locator, 8).context("malformed ZIP64 record offset")?;
            file.seek(SeekFrom::Start(record_offset))?;
            let mut record = [0_u8; 56];
            file.read_exact(&mut record)?;
            if &record[..4] != ZIP64_EOCD_SIGNATURE {
                bail!("artifact '{artifact_name}' has an invalid ZIP64 end record");
            }
            let record_size = le_u64(&record, 4).context("malformed ZIP64 record size")?;
            if record_size < 44 {
                bail!("artifact '{artifact_name}' has a truncated ZIP64 end record");
            }
            let record_end = record_offset
                .checked_add(12)
                .and_then(|value| value.checked_add(record_size))
                .context("ZIP64 end record range overflowed")?;
            if record_end > locator_offset {
                bail!("artifact '{artifact_name}' has an invalid ZIP64 end-record position");
            }
            let record_disk = le_u32(&record, 16).context("malformed ZIP64 disk number")?;
            let record_disk_with_directory =
                le_u32(&record, 20).context("malformed ZIP64 central-directory disk")?;
            let record_entries_on_disk =
                le_u64(&record, 24).context("malformed ZIP64 disk entry count")?;
            let record_entries = le_u64(&record, 32).context("malformed ZIP64 entry count")?;
            if record_disk != 0
                || record_disk_with_directory != 0
                || record_entries_on_disk != record_entries
            {
                bail!("artifact '{artifact_name}' is a multi-disk or inconsistent ZIP64 archive");
            }
            (
                ZipCentralDirectoryMetadata {
                    entries: record_entries,
                    size: le_u64(&record, 40).context("malformed ZIP64 central-directory size")?,
                    offset: le_u64(&record, 48)
                        .context("malformed ZIP64 central-directory offset")?,
                },
                record_offset,
            )
        };

    ensure_artifact_zip_member_limit(
        artifact_name,
        usize::try_from(metadata.entries).unwrap_or(usize::MAX),
        RESULTS_ARTIFACT_MAX_ZIP_MEMBERS,
    )?;
    ensure_artifact_size_limit(
        artifact_name,
        "ZIP central directory",
        metadata.size,
        RESULTS_ARTIFACT_MAX_ZIP_CENTRAL_DIRECTORY_BYTES,
        "split the artifact into smaller uploads",
    )?;
    let directory_end = metadata
        .offset
        .checked_add(metadata.size)
        .context("ZIP central-directory range overflowed")?;
    if directory_end != directory_end_offset {
        bail!(
            "artifact '{artifact_name}' has a central directory that is not adjacent to its end record"
        );
    }
    if directory_end > file_len || metadata.offset > file_len {
        bail!("artifact '{artifact_name}' has a central directory outside the ZIP");
    }

    let directory_size = usize::try_from(metadata.size)
        .context("ZIP central-directory size does not fit in memory")?;
    let mut directory = vec![0_u8; directory_size];
    file.seek(SeekFrom::Start(metadata.offset))?;
    file.read_exact(&mut directory)?;
    const CENTRAL_HEADER_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
    const CENTRAL_HEADER_BYTES: usize = 46;
    let mut cursor = 0_usize;
    let mut actual_entries = 0_u64;
    while cursor < directory.len() {
        let header = directory
            .get(cursor..cursor.saturating_add(CENTRAL_HEADER_BYTES))
            .context("truncated ZIP central-directory header")?;
        if &header[..4] != CENTRAL_HEADER_SIGNATURE {
            bail!("artifact '{artifact_name}' has an invalid ZIP central-directory header");
        }
        let name_len = usize::from(le_u16(header, 28).context("malformed ZIP file name length")?);
        let extra_len = usize::from(le_u16(header, 30).context("malformed ZIP extra length")?);
        let comment_len = usize::from(le_u16(header, 32).context("malformed ZIP comment length")?);
        let record_len = CENTRAL_HEADER_BYTES
            .checked_add(name_len)
            .and_then(|value| value.checked_add(extra_len))
            .and_then(|value| value.checked_add(comment_len))
            .context("ZIP central-directory record length overflowed")?;
        cursor = cursor
            .checked_add(record_len)
            .context("ZIP central-directory cursor overflowed")?;
        if cursor > directory.len() {
            bail!("artifact '{artifact_name}' has a truncated ZIP central-directory record");
        }
        actual_entries = actual_entries
            .checked_add(1)
            .context("ZIP central-directory entry count overflowed")?;
    }
    if actual_entries != metadata.entries {
        bail!(
            "artifact '{artifact_name}' central-directory entry count mismatch: expected {}, found {actual_entries}",
            metadata.entries
        );
    }
    Ok(Some(metadata))
}

pub(crate) fn validate_zip_central_directory_for_restore(
    file: &mut std::fs::File,
    artifact_name: &str,
) -> Result<Option<(u64, u64)>> {
    Ok(validate_zip_central_directory(file, artifact_name)?
        .map(|metadata| (metadata.entries, metadata.offset)))
}

#[derive(Debug, Clone)]
struct ResultsArtifactEntry {
    relative: std::path::PathBuf,
    is_directory: bool,
    uncompressed_size: u64,
}

fn preflight_results_artifact_zip<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    artifact_name: &str,
    total_returned_bytes: &mut u64,
) -> Result<Vec<ResultsArtifactEntry>> {
    ensure_artifact_zip_member_limit(
        artifact_name,
        archive.len(),
        RESULTS_ARTIFACT_MAX_ZIP_MEMBERS,
    )?;
    let mut entries = Vec::with_capacity(archive.len());
    let mut seen = BTreeSet::new();
    let mut file_paths = BTreeSet::new();
    let mut path_bytes = 0_u64;
    let mut zip_uncompressed_bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let Some(path) = entry.enclosed_name() else {
            bail!("artifact '{artifact_name}' contains an unsafe archive path");
        };
        if path.as_os_str().is_empty() || !seen.insert(path.to_path_buf()) {
            bail!(
                "artifact '{artifact_name}' contains a duplicate archive path: {}",
                path.display()
            );
        }
        let depth = path.components().count();
        if depth > RESULTS_ARTIFACT_MAX_ZIP_PATH_DEPTH {
            bail!(
                "artifact '{artifact_name}' path {} has depth {depth}, exceeding the {}-component limit",
                path.display(),
                RESULTS_ARTIFACT_MAX_ZIP_PATH_DEPTH
            );
        }
        path_bytes = checked_artifact_size_add(
            artifact_name,
            "ZIP path metadata",
            path_bytes,
            u64::try_from(path.as_os_str().as_encoded_bytes().len()).unwrap_or(u64::MAX),
            RESULTS_ARTIFACT_MAX_ZIP_PATH_BYTES,
            "split the artifact or reduce its path metadata",
        )?;
        let is_directory = entry.is_dir();
        let uncompressed_size = entry.size();
        if !is_directory {
            file_paths.insert(path.to_path_buf());
            zip_uncompressed_bytes = checked_artifact_size_add(
                artifact_name,
                "ZIP uncompressed payload",
                zip_uncompressed_bytes,
                uncompressed_size,
                RESULTS_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES,
                "split the artifact or reduce its uncompressed contents",
            )?;
            *total_returned_bytes = checked_artifact_size_add(
                artifact_name,
                "total selected download payload",
                *total_returned_bytes,
                uncompressed_size,
                RESULTS_ARTIFACT_MAX_TOTAL_RETURNED_BYTES,
                "narrow the artifact name or pattern selection",
            )?;
        }
        entries.push(ResultsArtifactEntry {
            relative: path.to_path_buf(),
            is_directory,
            uncompressed_size,
        });
    }
    for entry in entries.iter().filter(|entry| !entry.is_directory) {
        for ancestor in entry.relative.ancestors().skip(1) {
            if file_paths.contains(ancestor) {
                bail!(
                    "artifact '{artifact_name}' has a file ancestor conflict at {}",
                    ancestor.display()
                );
            }
        }
    }
    Ok(entries)
}

/// Download artifacts visible to this workflow run through the Results
/// Service v4 protocol used by `actions/download-artifact`.
///
/// `name` (exact) or `pattern` (glob) filter the artifact list BEFORE anything
/// is signed or downloaded. Selected artifacts are returned whether the
/// Results Service serves them as ZIP archives or raw blobs.
///
/// Flow: ListArtifacts -> GetSignedArtifactURL -> GET zip. Signed URLs and the
/// runtime bearer token are kept out of process arguments.
pub(crate) fn download_artifacts_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    pattern: &str,
) -> Result<Vec<ResultsArtifactDownload>> {
    download_artifacts_blocking_in_temp_dir(
        results_service_url,
        token,
        plan_id,
        job_id,
        name,
        pattern,
        &std::env::temp_dir(),
    )
}

/// Listing enumeration: per-row wire validation only, no identity contract.
///
/// The Results Service accepts repeated `(job, name)` rows instead of
/// rejecting them, so a listing can always carry duplicates — a strict read
/// contract would turn that service-side data condition into job failures,
/// which is exactly how run 34711777480's legacy cross-job `job-log` rows
/// rejected eleven downstream jobs. Reads tolerate duplicates; producers stay
/// idempotent (`overwrite: true`); destructive paths filter with
/// `artifacts_owned_by_job` and pick targets by artifact ID so they can also
/// repair a duplicated pair instead of aborting on it.
fn list_results_artifacts(
    client: &reqwest::blocking::Client,
    base: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
) -> Result<Vec<ValidatedResultsArtifactDescriptor>> {
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    let list_body = serde_json::to_string(&serde_json::json!({
        "workflow_run_backend_id": plan_id,
        "workflow_job_run_backend_id": job_id
    }))
    .context("serialize ListArtifacts")?;
    let list_url = format!("{base}/{SERVICE}/ListArtifacts");
    let listed_text = results_service_post(client, &list_url, token, &list_body, "ListArtifacts")?;
    let listed: ListArtifactsResponse =
        serde_json::from_str(&listed_text).context("parse Results Service ListArtifacts")?;
    let artifacts = listed
        .artifacts
        .context("Results Service ListArtifacts omitted artifacts")?;
    if artifacts.len() > RESULTS_ARTIFACT_MAX_LISTED_ARTIFACTS {
        bail!(
            "Results Service listed {} artifacts, exceeding the {}-artifact limit; narrow the workflow scope",
            artifacts.len(),
            RESULTS_ARTIFACT_MAX_LISTED_ARTIFACTS
        );
    }
    artifacts
        .into_iter()
        .map(|artifact| artifact.validate())
        .collect()
}

fn validate_selected_result_artifact_names(
    artifacts: &[ValidatedResultsArtifactDescriptor],
) -> Result<()> {
    let mut names = BTreeSet::new();
    for artifact in artifacts {
        if !names.insert(artifact.name.as_str()) {
            bail!(
                "Results Service returned duplicate selected artifact name '{}'",
                artifact.name
            );
        }
    }
    Ok(())
}

/// Restrict destructive artifact operations to artifacts produced by this
/// job. The general listing intentionally remains cross-job because Results
/// Service downloads support fan-in from parallel producer jobs.
fn artifacts_owned_by_job(
    artifacts: Vec<ValidatedResultsArtifactDescriptor>,
    job_id: &str,
) -> Vec<ValidatedResultsArtifactDescriptor> {
    artifacts
        .into_iter()
        .filter(|artifact| artifact.workflow_job_run_backend_id == job_id)
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
fn test_results_artifact_descriptor(
    job_id: &str,
    database_id: u64,
) -> ValidatedResultsArtifactDescriptor {
    named_test_results_artifact_descriptor(job_id, database_id, "release")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
fn named_test_results_artifact_descriptor(
    job_id: &str,
    database_id: u64,
    name: &str,
) -> ValidatedResultsArtifactDescriptor {
    ValidatedResultsArtifactDescriptor {
        workflow_run_backend_id: "plan".to_owned(),
        workflow_job_run_backend_id: job_id.to_owned(),
        database_id,
        name: name.to_owned(),
        size: 7,
        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_owned(),
    }
}

pub(crate) fn results_artifact_id_by_name_blocking(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
) -> Result<u64> {
    validate_results_artifact_name(name)?;
    let results_service_url = validate_known_service_url(
        results_service_url,
        "ResultsServiceUrl",
        &["results-receiver.actions.githubusercontent.com"],
    )?;
    let base = results_service_url.as_str().trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(RUNNER_USER_AGENT)
        .build()
        .context("build Results Service HTTP client")?;
    let matching = list_results_artifacts(&client, base, token, plan_id, job_id)?
        .into_iter()
        .filter(|artifact| artifact.name == name)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!(
            "expected exactly one Results Service artifact named '{name}', found {}",
            matching.len()
        );
    }
    Ok(matching[0].database_id)
}

fn download_artifacts_blocking_in_temp_dir(
    results_service_url: &str,
    token: &str,
    plan_id: &str,
    job_id: &str,
    name: &str,
    pattern: &str,
    tmp_dir: &std::path::Path,
) -> Result<Vec<ResultsArtifactDownload>> {
    const SERVICE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
    let results_service_url = validate_known_service_url(
        results_service_url,
        "ResultsServiceUrl",
        &["results-receiver.actions.githubusercontent.com"],
    )?;
    let base = results_service_url.as_str().trim_end_matches('/');

    let matcher = if !name.is_empty() || pattern.is_empty() {
        None
    } else {
        let mut builder = globset::GlobSetBuilder::new();
        builder.add(globset::Glob::new(pattern)?);
        Some(builder.build().context("build artifact pattern")?)
    };

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(RUNNER_USER_AGENT)
        .build()
        .context("build Results Service HTTP client")?;

    let artifacts = list_results_artifacts(&client, base, token, plan_id, job_id)?;
    // Filter before enforcing global name uniqueness. Legacy runs can contain
    // duplicate names from unrelated producer jobs; those must not poison a
    // narrow download. Selected duplicates remain ambiguous and fail closed.
    let selected_artifacts = artifacts
        .into_iter()
        .filter(|artifact| {
            let artifact_name = artifact.name.as_str();
            if !name.is_empty() {
                artifact_name == name
            } else if let Some(matcher) = &matcher {
                matcher.is_match(artifact_name)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    validate_selected_result_artifact_names(&selected_artifacts)?;
    let mut downloads = Vec::new();
    let mut total_returned_bytes = 0_u64;
    for artifact in selected_artifacts {
        let artifact_name = artifact.name.as_str();
        let signed_body = serde_json::to_string(&serde_json::json!({
            "workflow_run_backend_id": artifact.workflow_run_backend_id,
            "workflow_job_run_backend_id": artifact.workflow_job_run_backend_id,
            "name": artifact_name
        }))
        .context("serialize GetSignedArtifactURL")?;
        let signed_url_endpoint = format!("{base}/{SERVICE}/GetSignedArtifactURL");
        let signed_text = results_service_post(
            &client,
            &signed_url_endpoint,
            token,
            &signed_body,
            "GetSignedArtifactURL",
        )?;
        let signed: GetSignedArtifactUrlResponse = serde_json::from_str(&signed_text)
            .context("parse Results Service GetSignedArtifactURL")?;
        if signed.signed_url.is_empty() {
            bail!("GetSignedArtifactURL returned no signed URL");
        }
        let signed_url = signed.signed_url;
        let signed_url = validate_signed_blob_url(&signed_url, "artifact download")?;
        let artifact_path = tmp_dir.join(format!(
            "velnor-artifact-download-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let mut response = client
            .get(signed_url.as_str())
            .timeout(artifact_transfer_timeout(
                RESULTS_ARTIFACT_MAX_DOWNLOAD_RESPONSE_BYTES,
            ))
            .send()
            .map_err(|error| {
                anyhow::anyhow!(
                    "download Results Service artifact blob: {}",
                    redacted_reqwest_error(&error)
                )
            })?;
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            let body = read_bounded_response_preview(&mut response);
            bail!("artifact '{artifact_name}' download failed: status={status}, body={body}");
        }
        if let Some(content_length) = response.content_length() {
            ensure_artifact_size_limit(
                artifact_name,
                "download response Content-Length",
                content_length,
                RESULTS_ARTIFACT_MAX_DOWNLOAD_RESPONSE_BYTES,
                "split the artifact into smaller uploads",
            )?;
        }
        let response_headers = response.headers().clone();
        let content_type = response_headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let (mut artifact_path, mut output_file) =
            open_artifact_temp_file(artifact_path).context("create artifact zip temp file")?;
        copy_artifact_response_bounded(
            &mut response,
            &mut output_file,
            artifact_name,
            RESULTS_ARTIFACT_MAX_DOWNLOAD_RESPONSE_BYTES,
        )
        .context("write downloaded artifact zip")?;
        output_file
            .sync_all()
            .context("sync downloaded artifact zip")?;
        artifact_path.file = Some(output_file);
        let artifact_archive = Arc::new(artifact_path);
        let actual_size = artifact_archive
            .file
            .as_ref()
            .context("retain downloaded artifact for size verification")?
            .metadata()
            .context("stat downloaded artifact")?
            .len();
        if actual_size != artifact.size {
            bail!(
                "artifact '{artifact_name}' downloaded size {actual_size} does not match descriptor size {}",
                artifact.size
            );
        }
        let mut digest_file = artifact_archive
            .file
            .as_ref()
            .context("retain downloaded artifact for digest verification")?
            .try_clone()
            .context("duplicate downloaded artifact for digest verification")?;
        let actual_digest = hash_artifact_file(&mut digest_file)?;
        if !digest_matches(&artifact.digest, &actual_digest) {
            bail!(
                "artifact '{artifact_name}' downloaded digest sha256:{actual_digest} does not match descriptor digest {}",
                artifact.digest
            );
        }
        let is_zip = artifact_response_is_zip(content_type.as_deref(), signed_url.as_str());
        let mut validation_file = artifact_archive
            .file
            .as_ref()
            .context("retain downloaded artifact zip")?
            .try_clone()
            .context("duplicate downloaded artifact zip for metadata preflight")?;
        let validated_zip = validate_zip_central_directory(&mut validation_file, artifact_name)?;
        let archive_file = artifact_archive
            .file
            .as_ref()
            .context("retain downloaded artifact zip")?
            .try_clone()
            .context("duplicate downloaded artifact zip for ZIP parsing")?;
        let mut archive = match zip::ZipArchive::new(archive_file) {
            Ok(archive) => archive,
            Err(err) => {
                // Selected raw artifacts (for example a gzip `.dockerbuild`
                // build record) are valid Results Service artifacts. Preserve
                // their bytes under the artifact name; never silently drop a
                // selected artifact. A ZIP content type with invalid bytes is
                // still a protocol failure.
                if is_zip {
                    bail!("artifact '{artifact_name}' is not a valid ZIP archive: {err}");
                }
                let raw_size = artifact_archive
                    .file
                    .as_ref()
                    .context("retain downloaded raw artifact")?
                    .metadata()
                    .context("stat raw artifact")?
                    .len();
                ensure_artifact_size_limit(
                    artifact_name,
                    "raw payload",
                    raw_size,
                    RESULTS_ARTIFACT_MAX_RAW_BYTES,
                    "split the raw artifact into smaller uploads",
                )?;
                total_returned_bytes = checked_artifact_size_add(
                    artifact_name,
                    "total selected download payload",
                    total_returned_bytes,
                    raw_size,
                    RESULTS_ARTIFACT_MAX_TOTAL_RETURNED_BYTES,
                    "narrow the artifact name or pattern selection",
                )?;
                downloads.push(ResultsArtifactDownload {
                    name: artifact_name.to_string(),
                    files: vec![ResultsArtifactFile {
                        relative_path: raw_artifact_filename(&response_headers, artifact_name)?,
                        source: ResultsArtifactFileSource::Raw(artifact_archive.clone()),
                    }],
                });
                continue;
            }
        };
        let Some((validated_entries, validated_offset)) =
            validated_zip.map(|metadata| (metadata.entries, metadata.offset))
        else {
            bail!(
                "artifact '{artifact_name}' ZIP parser accepted input without a validated central directory"
            );
        };
        if u64::try_from(archive.len()).unwrap_or(u64::MAX) != validated_entries
            || archive.central_directory_start() != validated_offset
        {
            bail!("artifact '{artifact_name}' ZIP parser selected a different central directory");
        }
        let entries =
            preflight_results_artifact_zip(&mut archive, artifact_name, &mut total_returned_bytes)?;
        let staging = create_artifact_download_staging_directory(tmp_dir)?;
        let mut files = Vec::new();
        for (index, entry_metadata) in entries.into_iter().enumerate() {
            let mut entry = archive.by_index(index)?;
            if entry_metadata.is_directory {
                continue;
            }
            let path = entry_metadata.relative;
            staging
                .root
                .write_file_from_reader(&mut entry, &path, entry_metadata.uncompressed_size, 0o644)
                .with_context(|| {
                    format!(
                        "extract Results Service artifact '{artifact_name}' member {}",
                        path.display()
                    )
                })?;
            files.push(ResultsArtifactFile {
                relative_path: path.clone(),
                source: ResultsArtifactFileSource::Staged {
                    staging: staging.clone(),
                    relative: path.clone(),
                },
            });
        }
        downloads.push(ResultsArtifactDownload {
            name: artifact_name.to_string(),
            files,
        });
    }
    Ok(downloads)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn github_http_transport_accepts_only_explicit_values() {
        assert_eq!(parse_github_http_transport("native").unwrap(), "native");
        assert_eq!(parse_github_http_transport(" curl ").unwrap(), "curl");
        for value in ["", "reqwest", "unknown"] {
            let error = parse_github_http_transport(value).unwrap_err();
            assert!(error.to_string().contains("accepted values: native, curl"));
        }
    }

    #[test]
    fn curl_command_args_are_typed_bounded_and_keep_token_out_of_argv() {
        for method in ["POST", "PUT", "DELETE"] {
            let spec = curl_command_args(
                method,
                "https://api.github.com/repos/tailrocks/velnor",
                "ghs_test_token",
                Some("@{\"hello\":true}"),
                0,
                "application/vnd.github+json",
                Some("2026-03-10"),
            )
            .unwrap();
            let args = spec
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert!(args.windows(2).any(|pair| pair == ["--max-time", "1"]));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--connect-timeout", "1"]));
            assert!(args.windows(2).any(|pair| pair == ["--retry", "0"]));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--data-raw", "@{\"hello\":true}"]));
            assert!(!args.iter().any(|arg| arg.contains("ghs_test_token")));

            let headers = String::from_utf8(spec.header_stdin).unwrap();
            assert!(headers.contains("Authorization: Bearer ghs_test_token\n"));
            assert!(headers.contains("Content-Type: application/json\n"));
            assert!(headers.contains("X-GitHub-Api-Version: 2026-03-10\n"));
        }
    }

    #[test]
    fn parse_curl_response_extracts_final_status_headers_and_body() {
        let response = parse_curl_response(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 201 \r\nx-ratelimit-remaining: 41\r\nretry-after: 2\r\n\r\n{\"ok\":true}",
        )
        .unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.body, r#"{"ok":true}"#);
        assert_eq!(
            response
                .headers
                .get("x-ratelimit-remaining")
                .unwrap()
                .to_str()
                .unwrap(),
            "41"
        );
        assert_eq!(
            response
                .headers
                .get("retry-after")
                .unwrap()
                .to_str()
                .unwrap(),
            "2"
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn contents_request_types_native_and_curl_failures() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _transport_guard = runtime.block_on(crate::test_support::github_http_transport_env());
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let serve_response = |status: u16, body: Vec<u8>| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                let reason = match status {
                    200 => "OK",
                    503 => "Service Unavailable",
                    _ => "Test Response",
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                let _ = stream.write_all(&body);
            });
            (format!("http://{address}/contents"), server)
        };

        for transport in ["native", "curl"] {
            // The test owns the process-wide environment lock.
            unsafe { std::env::set_var(GITHUB_HTTP_TRANSPORT_ENV, transport) };

            let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let closed_url = format!("http://{}/contents", closed_listener.local_addr().unwrap());
            drop(closed_listener);
            let transport_error = match github_contents_request(&client, &closed_url, "token", 4) {
                Err(error) => error,
                Ok(_) => panic!("{transport} request to a closed port unexpectedly succeeded"),
            };
            assert!(
                matches!(&transport_error, GithubContentsRequestError::Transport(_)),
                "{transport} transport error: {transport_error}"
            );

            let (status_url, status_server) = serve_response(503, b"unavailable".to_vec());
            let status_response =
                github_contents_request(&client, &status_url, "token", 16).unwrap();
            status_server.join().unwrap();
            assert_eq!(status_response.status, 503, "{transport} status response");

            let (large_url, large_server) = serve_response(200, b"12345".to_vec());
            let large_error = match github_contents_request(&client, &large_url, "token", 4) {
                Err(error) => error,
                Ok(_) => panic!("{transport} oversized response unexpectedly succeeded"),
            };
            large_server.join().unwrap();
            assert_eq!(
                large_error,
                GithubContentsRequestError::BodyTooLarge {
                    max_body_bytes: 4,
                    status: Some(200),
                },
                "{transport} body limit"
            );

            let (utf8_url, utf8_server) = serve_response(200, vec![0xff]);
            let utf8_error = match github_contents_request(&client, &utf8_url, "token", 4) {
                Err(error) => error,
                Ok(_) => panic!("{transport} invalid UTF-8 response unexpectedly succeeded"),
            };
            utf8_server.join().unwrap();
            assert_eq!(
                utf8_error,
                GithubContentsRequestError::BodyInvalidUtf8 { status: Some(200) },
                "{transport} invalid UTF-8"
            );
        }

        // Invalid local transport configuration stays an internal failure.
        unsafe { std::env::set_var(GITHUB_HTTP_TRANSPORT_ENV, "unsupported") };
        let internal_error = match github_contents_request(&client, "not a URL", "token", 4) {
            Err(error) => error,
            Ok(_) => panic!("invalid transport configuration unexpectedly succeeded"),
        };
        assert!(matches!(
            &internal_error,
            GithubContentsRequestError::Internal(_)
        ));
    }

    #[tokio::test]
    async fn public_github_requests_validate_transport_before_io() {
        let _transport_guard = crate::test_support::github_http_transport_env().await;
        // SAFETY: the test holds the process-wide environment guard.
        unsafe { std::env::remove_var(GITHUB_HTTP_TRANSPORT_ENV) };

        let error = github_json_request("INVALID", "not a URL", "token", None, 1)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must be set to 'native' or 'curl'"),
            "{error:#}"
        );
        let error = github_json_request_with_rate_limit("INVALID", "not a URL", "token", None, 1)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must be set to 'native' or 'curl'"),
            "{error:#}"
        );

        // SAFETY: the test holds the process-wide environment guard.
        unsafe { std::env::set_var(GITHUB_HTTP_TRANSPORT_ENV, "unsupported") };
        let error = github_json_request("INVALID", "not a URL", "token", None, 1)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("accepted values: native, curl"),
            "{error:#}"
        );
        let error = github_json_request_with_rate_limit("INVALID", "not a URL", "token", None, 1)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("accepted values: native, curl"),
            "{error:#}"
        );
    }

    #[cfg(feature = "test-support")]
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

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn find_runner_group_stops_when_target_is_found() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let first_page: Vec<_> = (1..=100)
            .map(|id| serde_json::json!({ "id": id, "name": format!("group-{id}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .and(query_param("per_page", "100"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_count": 101,
                "runner_groups": first_page,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .and(query_param("per_page", "100"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_count": 101,
                "runner_groups": [{ "id": 101, "name": "velnor-trusted" }],
            })))
            .mount(&server)
            .await;

        let scope = GitHubScope::parse(&format!("{}/tailrocks", server.uri())).unwrap();
        let group = RegistrationClient::new()
            .unwrap()
            .find_runner_group(&scope, "test-token", "velnor-trusted", None)
            .await
            .unwrap();

        assert_eq!(group.id, 101);
        assert_eq!(group.name, "velnor-trusted");
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn jit_rate_limit_returns_without_waiting_for_reset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
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
    fn runner_delete_busy_message_nested_in_errors_array_is_conflict() {
        let body = r#"{"message":"Validation failed","errors":[{"resource":"Runner","code":"custom","message":"Sorry, the runner is currently running a job."}]}"#;
        assert!(runner_delete_is_busy_conflict(422, body));
        assert_eq!(
            classify_runner_delete(422, body),
            Some(RunnerDeleteOutcome::BusyConflict)
        );
    }

    #[test]
    fn runner_delete_busy_words_outside_message_fields_are_not_conflict() {
        // The vocabulary must match parsed message fields, never the raw
        // body: a URL or unrelated field carrying these words proves nothing.
        let body = r#"{"message":"Validation failed","documentation_url":"https://docs.github.com/runner_is_busy"}"#;
        assert!(!runner_delete_is_busy_conflict(422, body));
        assert_eq!(classify_runner_delete(422, body), None);
        // An unparseable 422 fails closed to the generic API error path.
        assert!(!runner_delete_is_busy_conflict(
            422,
            "currently running a job"
        ));
        assert_eq!(classify_runner_delete(422, "currently running a job"), None);
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
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-zip-test-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let (temp, size, hash) =
            write_artifact_zip_temp_file(path, &[("seed.tar.zst".into(), vec![42; 64])], true)
                .unwrap();
        let bytes = std::fs::read(temp.path()).unwrap();
        let expected_hash = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(hash, expected_hash);
        assert_eq!(
            std::fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let file = archive.by_index(0).unwrap();
        assert_eq!(file.compression(), zip::CompressionMethod::Stored);
    }

    #[test]
    fn artifact_path_writer_streams_source_into_zip() {
        let source_path = std::env::temp_dir().join(format!(
            "velnor-artifact-source-{}.bin",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&source_path, b"streamed artifact\n").unwrap();
        let zip_path = std::env::temp_dir().join(format!(
            "velnor-artifact-path-writer-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let source = ArtifactUploadFile {
            archive_path: "dist/output.txt".to_string(),
            source: ArtifactUploadSource::Opened(std::fs::File::open(&source_path).unwrap()),
            source_path: source_path.clone(),
        };
        let (zip_temp, _, _) =
            write_artifact_zip_from_files_temp_file(zip_path, &[source], false).unwrap();
        let bytes = std::fs::read(zip_temp.path()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut entry = archive.by_name("dist/output.txt").unwrap();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
        assert_eq!(content, "streamed artifact\n");
        drop(zip_temp);
        std::fs::remove_file(source_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn artifact_path_writer_rejects_non_regular_descriptor() {
        let root =
            std::env::temp_dir().join(format!("velnor-artifact-non-regular-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let zip_path = root.join("artifact.zip");
        let source = root.join("source.txt");

        let error = write_artifact_zip_from_files_temp_file(
            zip_path,
            vec![ArtifactUploadFile {
                archive_path: "source.txt".to_string(),
                source: ArtifactUploadSource::Opened(std::fs::File::open(&root).unwrap()),
                source_path: source,
            }]
            .as_slice(),
            false,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("not a regular file"),
            "{error:#}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_download_response_bound_stops_before_writing_excess_bytes() {
        let mut response = std::io::Cursor::new(b"12345".to_vec());
        let mut output = Vec::new();

        let error =
            copy_artifact_response_bounded(&mut response, &mut output, "release", 4).unwrap_err();

        assert!(error.to_string().contains("4-byte limit"), "{error:#}");
        assert!(output.is_empty());
    }

    #[test]
    fn results_service_control_response_bound_rejects_chunked_excess() {
        let mut response = std::io::Cursor::new(b"12345".to_vec());
        let error = read_bounded_response_body(&mut response, "ListArtifacts", 4).unwrap_err();

        assert!(error.to_string().contains("ListArtifacts"), "{error:#}");
        assert!(error.to_string().contains("4-byte limit"), "{error:#}");
    }

    #[test]
    fn zip_metadata_preflight_rejects_excess_zip64_members_before_parser() {
        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-zip64-preflight-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let mut zip64_end = [0_u8; 56];
        zip64_end[..4].copy_from_slice(b"PK\x06\x06");
        zip64_end[4..12].copy_from_slice(&44_u64.to_le_bytes());
        zip64_end[24..32].copy_from_slice(&100_001_u64.to_le_bytes());
        zip64_end[32..40].copy_from_slice(&100_001_u64.to_le_bytes());
        let mut locator = [0_u8; 20];
        locator[..4].copy_from_slice(b"PK\x06\x07");
        locator[8..16].copy_from_slice(&0_u64.to_le_bytes());
        locator[16..20].copy_from_slice(&1_u32.to_le_bytes());
        let mut eocd = [0_u8; 22];
        eocd[..4].copy_from_slice(b"PK\x05\x06");
        eocd[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        eocd[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        eocd[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&zip64_end);
        bytes.extend_from_slice(&locator);
        bytes.extend_from_slice(&eocd);
        let temp = write_artifact_temp_file(path, &bytes).unwrap();

        let mut file = std::fs::File::open(temp.path()).unwrap();
        let error = validate_zip_central_directory(&mut file, "release").unwrap_err();
        assert!(error.to_string().contains("100001"), "{error:#}");
    }

    #[test]
    fn zip_metadata_preflight_ignores_signature_inside_terminal_comment() {
        use std::io::Write;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("payload.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"payload").unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        let eocd = bytes.len() - 22;
        bytes[eocd + 20..eocd + 22].copy_from_slice(&4_u16.to_le_bytes());
        bytes.extend_from_slice(b"PK\x05\x06");

        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-eocd-comment-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let temp = write_artifact_temp_file(path, &bytes).unwrap();
        let mut file = std::fs::File::open(temp.path()).unwrap();
        assert!(validate_zip_central_directory(&mut file, "release")
            .unwrap()
            .is_some());
    }

    #[test]
    fn zip_metadata_preflight_rejects_multi_disk_eocd() {
        use std::io::Write;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("payload.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"payload").unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        let eocd = bytes.len() - 22;
        bytes[eocd + 4..eocd + 6].copy_from_slice(&1_u16.to_le_bytes());

        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-multi-disk-{}.zip",
            uuid::Uuid::new_v4()
        ));
        let temp = write_artifact_temp_file(path, &bytes).unwrap();
        let mut file = std::fs::File::open(temp.path()).unwrap();
        let error = validate_zip_central_directory(&mut file, "release").unwrap_err();
        assert!(error.to_string().contains("multi-disk"), "{error:#}");
    }

    #[test]
    fn artifact_zip_entry_bound_rejects_excess_uncompressed_bytes() {
        let mut entry = std::io::Cursor::new(b"12345".to_vec());
        let mut output = Vec::new();

        let error = copy_zip_entry_bounded(
            &mut entry,
            &mut output,
            "release",
            std::path::Path::new("dist/output.bin"),
            4,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("remaining 4-byte extraction allowance"),
            "{error:#}"
        );
    }

    #[test]
    fn artifact_member_and_returned_byte_bounds_are_actionable() {
        let member_error = ensure_artifact_zip_member_limit("release", 3, 2).unwrap_err();
        assert!(
            member_error.to_string().contains("split it into artifacts"),
            "{member_error:#}"
        );

        let size_error = checked_artifact_size_add(
            "release",
            "total selected download payload",
            3,
            2,
            4,
            "narrow the artifact name or pattern selection",
        )
        .unwrap_err();
        assert!(
            size_error
                .to_string()
                .contains("narrow the artifact name or pattern selection"),
            "{size_error:#}"
        );
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
    fn selected_artifact_names_must_be_unambiguous() {
        let same_name_different_jobs = vec![
            test_results_artifact_descriptor("producer", 1),
            test_results_artifact_descriptor("other-job", 2),
        ];
        assert!(validate_selected_result_artifact_names(&same_name_different_jobs).is_err());
    }

    #[test]
    fn artifact_create_request_uses_current_results_service_wire_shape() {
        let request = artifact_create_request(
            "plan",
            "job",
            "release",
            None,
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        assert_eq!(request["version"], 7);
        assert_eq!(request["mime_type"], serde_json::json!("application/zip"));
    }

    #[test]
    fn destructive_artifact_selection_excludes_other_jobs() {
        let selected = artifacts_owned_by_job(
            vec![
                test_results_artifact_descriptor("producer", 1),
                test_results_artifact_descriptor("other-job", 2),
            ],
            "producer",
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].database_id, 1);
        assert_eq!(selected[0].workflow_job_run_backend_id, "producer");
    }

    #[cfg(feature = "test-support")]
    fn mock_request_content_length(headers: &[u8]) -> Option<usize> {
        String::from_utf8_lossy(headers).lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn mock_request_content_length_header_name_is_case_insensitive() {
        assert_eq!(
            mock_request_content_length(b"POST /upload HTTP/1.1\r\ncontent-length: 17\r\n\r\n"),
            Some(17)
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn mock_request_waits_for_the_full_content_length() {
        let headers = b"POST /upload HTTP/1.1\r\nContent-Length: 4\r\n\r\n";
        let mut request = headers.to_vec();
        assert_eq!(mock_request_is_complete(&request), Some(false));
        request.extend_from_slice(b"body");
        assert_eq!(mock_request_is_complete(&request), Some(true));
    }

    #[cfg(feature = "test-support")]
    fn mock_request_is_complete(request: &[u8]) -> Option<bool> {
        let headers_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")?
            + 4;
        let content_length = mock_request_content_length(&request[..headers_end])?;
        Some(request.len() >= headers_end.checked_add(content_length)?)
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn artifact_upload_sends_finalize_hash_and_rejects_unsuccessful_finalize() {
        use std::io::{Read, Write};
        use std::net::{Shutdown, TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let probe_addr = listener.local_addr().unwrap();
        let base = format!("http://{probe_addr}");
        let server_base = base.clone();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count > 0 {
                        request.extend_from_slice(&buffer[..count]);
                    }
                    if mock_request_is_complete(&request) == Some(true) {
                        break;
                    }
                    assert!(count > 0, "request ended before the declared body arrived");
                }
                requests.push(request);
                let (status, body) = match index {
                    0 => (
                        "200 OK",
                        serde_json::json!({
                            "ok": true,
                            "signed_upload_url": format!("{server_base}/upload")
                        })
                        .to_string(),
                    ),
                    1 => ("201 Created", String::new()),
                    _ => (
                        "200 OK",
                        serde_json::json!({"ok": false, "artifact_id": "artifact-1"}).to_string(),
                    ),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            // Wait for the caller to finish. If a regression restores the old
            // reconciliation path, the client blocks on a fourth request and
            // this server answers it, allowing the assertion below to fail.
            // Otherwise the caller releases this accept with a sentinel after
            // the upload function returns.
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                if request[..count].starts_with(b"PROBE") {
                    break;
                }
                requests.push(request[..count].to_vec());
                let body = "unexpected request";
                write!(
                    stream,
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });

        let error = upload_artifact_blocking(
            &base,
            "runtime-token",
            "plan",
            "job",
            "release",
            &[("dist/output.txt".to_string(), b"artifact".to_vec())],
            ArtifactUploadOptions::default(),
        )
        .unwrap_err();
        let mut probe = TcpStream::connect(probe_addr).unwrap();
        probe.write_all(b"PROBE").unwrap();
        probe.shutdown(Shutdown::Both).unwrap();
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains("ok=false")),
            "{error:#}"
        );

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        let create = String::from_utf8_lossy(&requests[0]);
        assert!(create.contains("\"version\":7"));
        assert!(create.contains("\"mime_type\":\"application/zip\""));
        let finalize = String::from_utf8_lossy(&requests[2]);
        assert!(finalize.contains("\"hash\":\"sha256:"));
    }

    /// The overwrite path is what makes a repeated upload idempotent, so it
    /// must delete exactly the stale rows this job owns for that name and
    /// leave every other row in the run alone.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn overwrite_deletes_only_this_jobs_rows_for_the_name() {
        use crate::test_support::results_artifact_store::Store;

        let store = Store::mount().await;
        store.seed(11, "job-1", "job-log-job-1");
        store.seed(12, "job-1", "other-artifact");
        store.seed(13, "job-2", "job-log-job-1");

        let results_url = store.uri();
        let finalized = tokio::task::spawn_blocking(move || {
            upload_artifact_blocking(
                &results_url,
                "runtime-token",
                "plan-1",
                "job-1",
                "job-log-job-1",
                &[("job-log.txt".to_string(), b"log".to_vec())],
                ArtifactUploadOptions {
                    overwrite: true,
                    ..ArtifactUploadOptions::default()
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        let replaced_id = finalized.id.parse::<u64>().unwrap();
        assert_eq!(
            store.rows(),
            vec![
                (12, "job-1".to_owned(), "other-artifact".to_owned()),
                (13, "job-2".to_owned(), "job-log-job-1".to_owned()),
                (replaced_id, "job-1".to_owned(), "job-log-job-1".to_owned()),
            ],
            "only the stale row of this job for this name may be deleted"
        );
        assert_eq!(store.deleted_ids(), vec![11]);
    }

    /// The delete phase is the repair path for a duplicated `(job, name)`
    /// pair: it enumerates raw rows, so an overwrite upload succeeds and ends
    /// with exactly one row even when the listing it starts from is poisoned.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn overwrite_self_heals_a_duplicate_row_pair() {
        use crate::test_support::results_artifact_store::Store;

        let store = Store::mount().await;
        store.seed(11, "job-1", "job-log-job-1");
        store.seed(12, "job-1", "job-log-job-1");

        let results_url = store.uri();
        tokio::task::spawn_blocking(move || {
            upload_artifact_blocking(
                &results_url,
                "runtime-token",
                "plan-1",
                "job-1",
                "job-log-job-1",
                &[("job-log.txt".to_string(), b"log".to_vec())],
                ArtifactUploadOptions {
                    overwrite: true,
                    ..ArtifactUploadOptions::default()
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(store.rows().len(), 1, "the pair must collapse to one row");
        assert_eq!(store.deleted_ids(), vec![11, 12]);
        assert_eq!(
            store.puts(),
            1,
            "exactly one replacement blob must reach the store"
        );
    }

    /// Without `overwrite` the same upload stacks a second `(job, name)` row
    /// — listings then resolve that name ambiguously, so callers that may
    /// re-upload must pass `overwrite: true`.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn upload_without_overwrite_stacks_a_duplicate_row() {
        use crate::test_support::results_artifact_store::Store;

        let store = Store::mount().await;
        store.seed(11, "job-1", "job-log-job-1");

        let results_url = store.uri();
        tokio::task::spawn_blocking(move || {
            upload_artifact_blocking(
                &results_url,
                "runtime-token",
                "plan-1",
                "job-1",
                "job-log-job-1",
                &[("job-log.txt".to_string(), b"log".to_vec())],
                ArtifactUploadOptions::default(),
            )
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(store.deleted_ids(), Vec::<u64>::new());
        assert_eq!(store.rows().len(), 2, "duplicate row accumulated");
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn artifact_upload_preparation_failure_sends_no_create_request() {
        use std::io::{ErrorKind, Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server_base = base.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = 0;
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        requests += 1;
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request);
                        let body = serde_json::json!({
                            "ok": true,
                            "signed_upload_url": format!("{server_base}/upload")
                        })
                        .to_string();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if stop_rx.recv_timeout(Duration::from_millis(10)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("artifact test server failed: {error}"),
                }
            }
            requests
        });

        let missing_source = std::env::temp_dir().join(format!(
            "velnor-missing-artifact-source-{}",
            uuid::Uuid::new_v4()
        ));
        let error = upload_artifact_files_blocking(
            &base,
            "runtime-token",
            "plan",
            "job",
            "release",
            vec![ArtifactUploadFile {
                archive_path: "dist/output.txt".to_string(),
                source: ArtifactUploadSource::Opened(std::fs::File::open(".").unwrap()),
                source_path: missing_source,
            }],
            ArtifactUploadOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("artifact source"), "{error:#}");

        stop_tx.send(()).unwrap();
        assert_eq!(server.join().unwrap(), 0);
    }

    #[test]
    fn artifact_zip_classification_matches_current_toolkit_variants() {
        for content_type in [
            "application/zip",
            "application/x-zip-compressed; charset=binary",
            "APPLICATION/ZIP-COMPRESSED",
        ] {
            assert!(artifact_response_is_zip(
                Some(content_type),
                "https://blob.test/data"
            ));
        }
        assert!(artifact_response_is_zip(
            Some("application/octet-stream"),
            "https://blob.test/data/archive.ZIP?sig=secret"
        ));
        assert!(!artifact_response_is_zip(
            Some("application/octet-stream"),
            "https://blob.test/data/archive.bin?sig=secret"
        ));
    }

    #[test]
    fn raw_artifact_filename_prefers_content_disposition_and_sanitizes_path() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"../report.json\"".parse().unwrap(),
        );
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("report.json")
        );

        headers.remove(reqwest::header::CONTENT_DISPOSITION);
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("fallback.bin")
        );
    }

    #[test]
    fn raw_artifact_filename_prefers_and_decodes_rfc5987_filename() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=old.txt; filename*=UTF-8''report%20%E2%9C%93.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("report ✓.txt")
        );
    }

    #[test]
    fn raw_artifact_filename_rejects_malformed_extended_value_and_falls_back() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=report.txt; filename*=UTF-8''bad%ZZname.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("report.txt")
        );

        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename*=ISO-8859-1''report.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("fallback.bin")
        );
    }

    #[test]
    fn raw_artifact_filename_sanitizes_decoded_traversal() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename*=UTF-8''..%2F..%2Fsecret.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            raw_artifact_filename(&headers, "fallback.bin").unwrap(),
            std::path::PathBuf::from("secret.txt")
        );
    }

    #[test]
    fn artifact_download_requires_exact_http_200() {
        assert!(artifact_download_status_is_ok(reqwest::StatusCode::OK));
        assert!(!artifact_download_status_is_ok(
            reqwest::StatusCode::PARTIAL_CONTENT
        ));
        assert!(!artifact_download_status_is_ok(
            reqwest::StatusCode::NO_CONTENT
        ));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn artifact_download_rejects_non_200_signed_responses_through_request_path() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        for status in [
            reqwest::StatusCode::PARTIAL_CONTENT,
            reqwest::StatusCode::NO_CONTENT,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server_base = base.clone();
            let status_line = format!("{} {}", status.as_u16(), status.canonical_reason().unwrap());
            let server = std::thread::spawn(move || {
                for index in 0..3 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request).unwrap();
                    let body = match index {
                        0 => serde_json::json!({
                            "artifacts": [{"name": "release", "workflow_run_backend_id": "plan", "workflow_job_run_backend_id": "consumer", "database_id": 1, "size": 4, "digest": format!("sha256:{}", sha2::Sha256::digest(b"x").iter().map(|b| format!("{b:02x}")).collect::<String>())}]
                        })
                        .to_string(),
                        1 => serde_json::json!({"signed_url": format!("{server_base}/signed.zip")}).to_string(),
                        _ => String::new(),
                    };
                    let response_status = if index == 2 { &status_line } else { "200 OK" };
                    write!(
                        stream,
                        "HTTP/1.1 {response_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            });

            let error = download_artifacts_blocking(
                &base,
                "runtime-token",
                "plan",
                "consumer",
                "release",
                "",
            )
            .unwrap_err();
            assert!(error.to_string().contains(&format!("status={status}")));
            server.join().unwrap();
        }
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_listing_keeps_rows_from_any_backend_id() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Rows are consumed as the server returns them, whatever backend IDs
        // they carry: a job redelivered under a new scope still lists the
        // artifacts its run wrote under the original scope, and follow-up
        // requests must echo each row's own IDs. Validating the row's
        // well-formedness stays; comparing IDs against the caller's token
        // scope was the deviation that turned this listing into zero rows.
        let digest = format!("sha256:{}", "1".repeat(64));
        let listing = serde_json::json!({
            "artifacts": [
                {
                    "name": "job-log-attempt-1",
                    "workflow_run_backend_id": "plan-attempt-1",
                    "workflow_job_run_backend_id": "job-attempt-1",
                    "database_id": 101,
                    "size": 3,
                    "digest": digest
                },
                {
                    "name": "velnor-ci-selection",
                    "workflow_run_backend_id": "plan-attempt-1",
                    "workflow_job_run_backend_id": "planning-job",
                    "database_id": 102,
                    "size": 3,
                    "digest": digest
                },
                {
                    "name": "release-linux",
                    "workflow_run_backend_id": "plan-attempt-2",
                    "workflow_job_run_backend_id": "job-attempt-2",
                    "database_id": 103,
                    "size": 3,
                    "digest": digest
                }
            ]
        })
        .to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
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
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{listing}",
                listing.len()
            )
            .unwrap();
            String::from_utf8_lossy(&request).to_string()
        });

        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .unwrap();

        // The queried IDs are the redelivered attempt's scope; every row was
        // written under the original attempt's scope.
        let listed = list_results_artifacts(
            &client,
            &base,
            "runtime-token",
            "plan-attempt-2",
            "job-attempt-2",
        )
        .unwrap();
        let mut listed = listed;
        listed.sort_by_key(|artifact| artifact.database_id);
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].name, "job-log-attempt-1");
        assert_eq!(listed[0].workflow_run_backend_id, "plan-attempt-1");
        assert_eq!(listed[0].workflow_job_run_backend_id, "job-attempt-1");
        assert_eq!(listed[1].name, "velnor-ci-selection");
        assert_eq!(listed[1].workflow_job_run_backend_id, "planning-job");
        assert_eq!(listed[2].workflow_run_backend_id, "plan-attempt-2");

        let request = server.join().unwrap();
        assert!(request.contains("ArtifactService/ListArtifacts"));
        assert!(request.contains("\"workflow_run_backend_id\":\"plan-attempt-2\""));
        assert!(request.contains("\"workflow_job_run_backend_id\":\"job-attempt-2\""));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_listing_rejects_malformed_rows() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Dropping the plan-ID gate keeps structural validation: a row with a
        // malformed digest is a broken server response, not a skippable row.
        let listing = serde_json::json!({
            "artifacts": [{
                "name": "velnor-ci-selection",
                "workflow_run_backend_id": "plan-attempt-1",
                "workflow_job_run_backend_id": "planning-job",
                "database_id": 102,
                "size": 3,
                "digest": "not-a-digest"
            }]
        })
        .to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
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
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{listing}",
                listing.len()
            )
            .unwrap();
        });

        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .unwrap();
        let error = list_results_artifacts(
            &client,
            &base,
            "runtime-token",
            "plan-attempt-2",
            "job-attempt-2",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must use the sha256:<64 hex digits> format"),
            "{error:#}"
        );
        server.join().unwrap();
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_download_finds_artifact_listed_under_a_redelivered_scope() {
        // Live failure (fleet build 0.1.274~preview.36, run 34723758941, job
        // "Docker · Docker / Velnor"): the job was redelivered 79 minutes after
        // its first attempt with a new run-service job ID and a token whose
        // `Actions.Results` scope no longer matched the backend IDs stored on
        // the run's artifact rows. The listing returned `velnor-ci-selection`,
        // the plan-ID gate discarded it as foreign, and the step reported
        // "Downloaded 0 artifact(s)". actions/toolkit selects rows by name and
        // signs with the row's own IDs, so the redelivered scope must not
        // influence what a download can see.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("selection.json", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"ci-selection\n").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        let digest = format!(
            "sha256:{}",
            sha2::Sha256::digest(&zip_bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
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
                let (content_type, body): (&str, Vec<u8>) = match index {
                    0 => (
                        "application/json",
                        serde_json::json!({
                            "artifacts": [
                                {
                                    "name": "job-log-attempt-1",
                                    "workflow_run_backend_id": "plan-attempt-1",
                                    "workflow_job_run_backend_id": "job-attempt-1",
                                    "database_id": 101,
                                    "size": zip_bytes.len(),
                                    "digest": digest
                                },
                                {
                                    "name": "velnor-ci-selection",
                                    "workflow_run_backend_id": "plan-attempt-1",
                                    "workflow_job_run_backend_id": "planning-job",
                                    "database_id": 102,
                                    "size": zip_bytes.len(),
                                    "digest": digest
                                }
                            ]
                        })
                        .to_string()
                        .into_bytes(),
                    ),
                    1 => (
                        "application/json",
                        serde_json::json!({"signed_url": signed_url})
                            .to_string()
                            .into_bytes(),
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

        // Redelivered job scope: plan-attempt-2 / job-attempt-2.
        let downloads = download_artifacts_blocking(
            &base,
            "runtime-token",
            "plan-attempt-2",
            "job-attempt-2",
            "velnor-ci-selection",
            "",
        )
        .unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].name, "velnor-ci-selection");
        let mut downloaded = Vec::new();
        downloads[0].files[0]
            .file()
            .unwrap()
            .read_to_end(&mut downloaded)
            .unwrap();
        assert_eq!(downloaded, b"ci-selection\n");

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        // GetSignedArtifactURL echoes the row's own backend IDs, not the
        // caller's token scope.
        assert!(requests[1].contains("\"workflow_run_backend_id\":\"plan-attempt-1\""));
        assert!(requests[1].contains("\"workflow_job_run_backend_id\":\"planning-job\""));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn artifact_overwrite_deletes_only_this_jobs_rows() {
        // Destructive paths stay scoped to this job's artifacts by job ID, not
        // by plan ID: a same-named row from another job (a legacy cross-job
        // `job-log`, or a redelivered attempt) must survive the overwrite.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let probe_addr = listener.local_addr().unwrap();
        let base = format!("http://{probe_addr}");
        let server_base = base.clone();
        let listing = serde_json::json!({
            "artifacts": [
                {
                    "name": "release",
                    "workflow_run_backend_id": "plan-attempt-1",
                    "workflow_job_run_backend_id": "job-attempt-1",
                    "database_id": 101,
                    "size": 3,
                    "digest": format!("sha256:{}", "1".repeat(64))
                },
                {
                    "name": "release",
                    "workflow_run_backend_id": "plan-attempt-2",
                    "workflow_job_run_backend_id": "job-attempt-2",
                    "database_id": 102,
                    "size": 3,
                    "digest": format!("sha256:{}", "1".repeat(64))
                }
            ]
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
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
                requests.push(String::from_utf8_lossy(&request).to_string());
                let (status, body): (&str, String) = match index {
                    0 => ("200 OK", listing.clone()),
                    // Answer 102, the only row this job owns. Deleting the
                    // redelivered attempt's row (101) would fail the client
                    // with an ID mismatch.
                    1 => (
                        "200 OK",
                        serde_json::json!({"ok": true, "artifact_id": "102"}).to_string(),
                    ),
                    2 => (
                        "200 OK",
                        serde_json::json!({
                            "ok": true,
                            "signed_upload_url": format!("{server_base}/upload")
                        })
                        .to_string(),
                    ),
                    3 => ("201 Created", String::new()),
                    _ => (
                        "200 OK",
                        serde_json::json!({"ok": true, "artifact_id": "103"}).to_string(),
                    ),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            // A sixth client request is a regression: record it.
            let mut extra = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                if request[..count].starts_with(b"PROBE") {
                    break;
                }
                extra.push(request[..count].to_vec());
                let body = "unexpected destructive request";
                write!(
                    stream,
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            (requests, extra)
        });

        upload_artifact_blocking(
            &base,
            "runtime-token",
            "plan-attempt-2",
            "job-attempt-2",
            "release",
            &[("dist/output.txt".to_string(), b"artifact".to_vec())],
            ArtifactUploadOptions {
                overwrite: true,
                ..ArtifactUploadOptions::default()
            },
        )
        .unwrap();

        let mut probe = TcpStream::connect(probe_addr).unwrap();
        probe.write_all(b"PROBE").unwrap();
        // The server exits and drops this accepted socket as soon as it reads
        // PROBE. Drop the client-owned stream instead of racing that peer
        // close with a fallible shutdown syscall.
        drop(probe);
        let (requests, extra) = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[0].contains("ArtifactService/ListArtifacts"));
        assert!(requests[1].contains("ArtifactService/DeleteArtifact"));
        assert!(requests[2].contains("ArtifactService/CreateArtifact"));
        assert!(requests[4].contains("ArtifactService/FinalizeArtifact"));
        assert!(
            extra.is_empty(),
            "overwrite must not delete rows owned by another job"
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_download_rejects_zip_path_traversal() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("../escape.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"must not escape").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let signed_url = format!("{base}/signed.zip");
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                let (content_type, body) = match index {
                    0 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({
                            "artifacts": [{"name": "release", "workflow_run_backend_id": "plan", "workflow_job_run_backend_id": "job", "database_id": 1, "size": zip_bytes.len(), "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())}]
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
        });

        let error =
            download_artifacts_blocking(&base, "runtime-token", "plan", "job", "release", "")
                .unwrap_err();
        assert!(
            error.to_string().contains("unsafe archive path"),
            "{error:#}"
        );
        server.join().unwrap();
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_download_cleans_temp_file_after_copy_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let temp_root = std::env::temp_dir().join(format!(
            "velnor-artifact-download-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&temp_root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let signed_url = format!("{base}/signed.zip");
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                let (content_type, body, content_length) = match index {
                    0 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({
                            "artifacts": [{"name": "release", "workflow_run_backend_id": "plan", "workflow_job_run_backend_id": "job", "database_id": 1, "size": 18, "digest": format!("sha256:{}", sha2::Sha256::digest(b"truncated artifact").iter().map(|b| format!("{b:02x}")).collect::<String>())}]
                        }))
                        .unwrap(),
                        None,
                    ),
                    1 => (
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({"signed_url": signed_url})).unwrap(),
                        None,
                    ),
                    _ => ("application/zip", b"truncated artifact".to_vec(), Some(1024)),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    content_length.unwrap_or(body.len())
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });

        let error = download_artifacts_blocking_in_temp_dir(
            &base,
            "runtime-token",
            "plan",
            "job",
            "release",
            "",
            &temp_root,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("write downloaded artifact zip"),
            "{error:#}"
        );
        server.join().unwrap();
        assert!(temp_root.read_dir().unwrap().next().is_none());
        std::fs::remove_dir(temp_root).unwrap();
    }

    #[cfg(feature = "test-support")]
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
                                "workflow_job_run_backend_id": "producer",
                                "database_id": 11,
                                "size": zip_bytes.len(),
                                "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
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
        assert_eq!(downloads[0].files.len(), 1);
        assert_eq!(
            downloads[0].files[0].relative_path,
            std::path::PathBuf::from("dist/output.txt")
        );
        let mut downloaded = Vec::new();
        downloads[0].files[0]
            .file()
            .unwrap()
            .read_to_end(&mut downloaded)
            .unwrap();
        assert_eq!(downloaded, b"artifact-v4\n");
        let requests = server.join().unwrap();
        assert!(requests[0].contains("ArtifactService/ListArtifacts"));
        assert!(requests[1].contains("ArtifactService/GetSignedArtifactURL"));
        assert!(requests[1].contains("\"workflow_job_run_backend_id\":\"producer\""));
        assert!(requests[2].starts_with("GET /signed.zip?credential=secret HTTP/1.1"));
    }

    #[cfg(feature = "test-support")]
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
                                    "workflow_job_run_backend_id": "producer",
                                    "database_id": 21,
                                    "size": zip_bytes.len(),
                                    "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
                                },
                                {
                                    "name": ".dockerbuild",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "image",
                                    "database_id": 22,
                                    "size": zip_bytes.len(),
                                    "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
                                },
                                {
                                    "name": ".dockerbuild",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "image-2",
                                    "database_id": 23,
                                    "size": zip_bytes.len(),
                                    "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
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

    #[cfg(feature = "test-support")]
    #[test]
    fn results_service_download_preserves_selected_raw_artifacts() {
        // Regression: an unfiltered download-all (merge-multiple) preserves a
        // non-zip `.dockerbuild` build-record artifact instead of silently
        // dropping it after ZIP parsing fails.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("dist/output.txt", zip::write::FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"artifact-v4\n").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        // .dockerbuild build records are gzip blobs, not zips.
        let gzip_bytes = b"\x1f\x8b\x08\x00dockerbuild-record-not-a-zip".to_vec();
        let expected_gzip_bytes = gzip_bytes.clone();
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
                                    "workflow_job_run_backend_id": "consumer",
                                    "database_id": 21,
                                    "size": zip_bytes.len(),
                                    "digest": format!("sha256:{}", sha2::Sha256::digest(&zip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
                                },
                                {
                                    "name": ".dockerbuild",
                                    "workflow_run_backend_id": "plan",
                                    "workflow_job_run_backend_id": "consumer",
                                    "database_id": 22,
                                    "size": gzip_bytes.len(),
                                    "digest": format!("sha256:{}", sha2::Sha256::digest(&gzip_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
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
        assert_eq!(downloads.len(), 2);
        assert_eq!(downloads[0].name, "release-linux");
        assert_eq!(downloads[0].files.len(), 1);
        assert_eq!(
            downloads[0].files[0].relative_path,
            std::path::PathBuf::from("dist/output.txt")
        );
        let mut downloaded = Vec::new();
        downloads[0].files[0]
            .file()
            .unwrap()
            .read_to_end(&mut downloaded)
            .unwrap();
        assert_eq!(downloaded, b"artifact-v4\n");
        assert_eq!(downloads[1].name, ".dockerbuild");
        assert_eq!(downloads[1].files.len(), 1);
        assert_eq!(
            downloads[1].files[0].relative_path,
            std::path::PathBuf::from(".dockerbuild")
        );
        let mut downloaded = Vec::new();
        downloads[1].files[0]
            .file()
            .unwrap()
            .read_to_end(&mut downloaded)
            .unwrap();
        assert_eq!(downloaded, expected_gzip_bytes);
    }

    #[test]
    fn artifact_temp_file_removes_path_when_guard_drops() {
        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-cleanup-{}.tmp",
            uuid::Uuid::new_v4()
        ));
        {
            let (_guard, _file) = open_artifact_temp_file(path.clone()).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn artifact_temp_file_collision_preserves_preexisting_file() {
        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-collision-{}.tmp",
            uuid::Uuid::new_v4()
        ));
        let original = b"file owned by another operation";
        std::fs::write(&path, original).unwrap();

        let error = match write_artifact_temp_file(path.clone(), b"replacement") {
            Ok(_) => panic!("collision unexpectedly accepted"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn artifact_temp_file_failed_open_does_not_leave_bytes() {
        let path = std::env::temp_dir().join(format!(
            "velnor-artifact-missing-parent-{}/artifact.zip",
            uuid::Uuid::new_v4()
        ));
        let error = match write_artifact_temp_file(path.clone(), b"secret artifact bytes") {
            Ok(_) => panic!("missing parent unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(error.kind() == std::io::ErrorKind::NotFound);
        assert!(!path.exists());
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
        // A transport failure without an HTTP status must be an error too.
        assert_eq!(classify_broker_poll(0, ""), BrokerPollClass::Error);
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
        // Transport/5xx/status-less failures retry; throttling retries.
        assert!(is_retriable_completion_status(0));
        assert!(is_retriable_completion_status(500));
        assert!(is_retriable_completion_status(502));
        assert!(is_retriable_completion_status(408));
        assert!(is_retriable_completion_status(429));
        // Deterministic 4xx must not retry.
        assert!(!is_retriable_completion_status(400));
        assert!(!is_retriable_completion_status(401));
        assert!(!is_retriable_completion_status(404));
        assert!(is_retriable_completion_status(409));
        assert!(!is_retriable_completion_status(422));
    }

    /// Build a run-service error body from the upstream contract, not from
    /// Velnor's parser. Field names are transcribed from
    /// `actions/runner@v2.337.0 src/Sdk/RSWebApi/Contracts/RunServiceError.cs`:
    ///
    /// ```text
    /// [DataMember(Name = "source",       EmitDefaultValue = false)] public string Source
    /// [DataMember(Name = "statusCode",   EmitDefaultValue = false)] public int    Code
    /// [DataMember(Name = "errorMessage", EmitDefaultValue = false)] public string Message
    /// ```
    ///
    /// Every fixture in this module must come through here so a fixture can
    /// never be derived from Velnor's own field names.
    fn upstream_run_service_error_body(
        source: &str,
        status_code: u16,
        message: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "source": source,
            "statusCode": status_code,
            "errorMessage": message,
        })
    }

    #[test]
    fn run_service_error_uses_upstream_wire_names() {
        let parsed: RunServiceError = serde_json::from_value(upstream_run_service_error_body(
            "actions-run-service",
            404,
            "Job not found",
        ))
        .unwrap();
        assert_eq!(parsed.source.as_deref(), Some("actions-run-service"));
        assert_eq!(parsed.code, Some(404));
        assert_eq!(parsed.message.as_deref(), Some("Job not found"));

        // The C# *property* names are `Code` and `Message`; the wire names are
        // `statusCode` and `errorMessage`. Reading the property spellings must
        // stay impossible, or a 404 acknowledgement silently degrades into a
        // permanent failure again.
        let property_named: RunServiceError = serde_json::from_str(
            r#"{"source":"actions-run-service","code":404,"message":"Job not found"}"#,
        )
        .unwrap();
        assert_eq!(property_named.code, None);
        assert_eq!(property_named.message, None);
        assert!(!is_run_service_job_not_found(
            r#"{"source":"actions-run-service","code":404,"message":"Job not found"}"#
        ));

        let case_insensitive_and_coerced = RunServiceError::parse(
            r#"{"SOURCE":"actions-run-service","STATUSCODE":"404","ERRORMESSAGE":"gone"}"#,
        )
        .expect("Json.NET matches contract member names without case sensitivity");
        assert_eq!(case_insensitive_and_coerced.code, Some(404));
        assert!(is_run_service_job_not_found(
            r#"{"SOURCE":"actions-run-service","STATUSCODE":"404"}"#
        ));
        assert_eq!(
            classify_acquire_job_response(
                500,
                r#"{"SOURCE":"actions-run-service","STATUSCODE":"404"}"#
            ),
            AcquireJobResponseClass::Skipped(AcquireJobSkipReason::NotFound)
        );

        assert_eq!(
            run_service_error_code(
                r#"{"source":"actions-run-service","statusCode":500,"STATUSCODE":404}"#
            ),
            Some(404),
            "the last case-insensitive duplicate member wins"
        );
        assert_eq!(
            run_service_error_code(
                r#"{"source":"actions-run-service","statusCode":"bad","STATUSCODE":404}"#
            ),
            None,
            "an invalid earlier occurrence fails before a later duplicate"
        );
    }

    #[test]
    fn permanent_completion_failures_are_told_apart_from_unlucky_ones() {
        let permanent = github_api_error("complete run-service job", 422, "invalid payload");
        assert!(completion_failure_is_permanent(&permanent));
        let permanent = permanent.context("complete run-service job after credential refresh");
        assert!(
            completion_failure_is_permanent(&permanent),
            "the classification must survive the context the callers add"
        );

        let retriable = github_api_error("complete run-service job", 503, "try later");
        assert!(!completion_failure_is_permanent(&retriable));
        assert!(!completion_failure_is_permanent(&github_api_error(
            "complete run-service job",
            409,
            "conflict"
        )));
        // Not knowing the remote's answer is exactly when retrying is right.
        assert!(!completion_failure_is_permanent(&anyhow::anyhow!(
            "connection reset"
        )));
    }

    #[test]
    fn completion_permanence_prefers_the_boundary_category() {
        // Policy-precedence unit over synthetic errors: the boundary verdict
        // wins over status re-derivation in both directions (a `Terminal`
        // refusal with a retriable-looking status is permanent, and a
        // non-terminal category with a refusal status is not). That the loop
        // actually produces these categories is proven by the wiremock tests
        // below, not here.
        let boundary_terminal = github_api_error_categorized(
            "complete run-service job",
            503,
            "try later",
            BrokerErrorCategory::Terminal,
        );
        assert!(completion_failure_is_permanent(&boundary_terminal));
        let wrapped = boundary_terminal.context("complete run-service job");
        assert!(
            completion_failure_is_permanent(&wrapped),
            "the boundary category must survive the context the callers add"
        );

        let boundary_transient = github_api_error_categorized(
            "complete run-service job",
            400,
            "bad request",
            BrokerErrorCategory::Transient,
        );
        assert!(!completion_failure_is_permanent(&boundary_transient));

        // Unclassified errors keep the historical status derivation.
        assert!(completion_failure_is_permanent(&github_api_error(
            "complete run-service job",
            400,
            "bad request"
        )));
        assert!(!completion_failure_is_permanent(&github_api_error(
            "complete run-service job",
            503,
            "try later"
        )));
    }

    #[test]
    fn acquire_reply_is_gone_only_for_typed_404() {
        assert!(acquire_reply_is_definitely_gone(
            &upstream_run_service_error_body("actions-run-service", 404, "gone").to_string(),
        ));
        // A conflict says the job is held. Upstream's error envelope carries no
        // runner identity, so it cannot say *by whom* — abandoning here would
        // drop a job this runner may have acquired before it crashed.
        assert!(!acquire_reply_is_definitely_gone(
            &upstream_run_service_error_body("actions-run-service", 409, "already acquired")
                .to_string(),
        ));
        // 422 is a distinct unprocessable response, not proof that the job is
        // gone or that this runner never acquired it.
        assert!(!acquire_reply_is_definitely_gone(
            &upstream_run_service_error_body("actions-run-service", 422, "unprocessable")
                .to_string(),
        ));
        // Raw HTTP status is never the oracle: an untyped or foreign-sourced
        // body proves nothing, whatever the outer status was.
        assert!(!acquire_reply_is_definitely_gone(
            r#"{"message":"Not Found"}"#
        ));
        assert!(!acquire_reply_is_definitely_gone(
            &upstream_run_service_error_body("actions-broker", 404, "gone").to_string(),
        ));
        assert!(!acquire_reply_is_definitely_gone(""));
    }

    #[test]
    fn acquire_skipped_category_is_terminal_only_for_typed_gone() {
        assert_eq!(
            classify_acquire_skipped(
                &upstream_run_service_error_body("actions-run-service", 404, "gone").to_string()
            ),
            BrokerErrorCategory::Terminal
        );
        // Held-by-unknown and unprocessable typed replies are non-retriable
        // upstream errors, but neither proves the request is gone.
        for (code, reason) in [
            (409u16, AcquireJobSkipReason::AlreadyAcquired),
            (422u16, AcquireJobSkipReason::Unprocessable),
        ] {
            let body =
                upstream_run_service_error_body("actions-run-service", code, "held or refused")
                    .to_string();
            assert_eq!(
                acquire_job_skip_reason(&body),
                Some(reason),
                "typed response code {code} must remain distinct"
            );
            assert_eq!(
                classify_acquire_skipped(&body),
                BrokerErrorCategory::Conflict
            );
        }
        // Untyped, foreign-sourced, and empty replies are unproven too.
        for body in [
            r#"{"message":"Not Found"}"#.to_owned(),
            upstream_run_service_error_body("actions-broker", 404, "gone").to_string(),
            String::new(),
        ] {
            assert_eq!(
                classify_acquire_skipped(&body),
                BrokerErrorCategory::Conflict,
                "unproven reply must not abandon: {body:?}"
            );
        }
    }

    #[test]
    fn broker_ack_errors_classify_for_forensics() {
        for status in [0u16, 408, 429, 500, 503] {
            assert_eq!(
                classify_broker_ack_error(status),
                BrokerErrorCategory::Transient,
                "status {status}"
            );
        }
        assert_eq!(
            classify_broker_ack_error(409),
            BrokerErrorCategory::Conflict
        );
        for status in [400u16, 401, 403, 404, 422] {
            assert_eq!(
                classify_broker_ack_error(status),
                BrokerErrorCategory::Terminal,
                "status {status}"
            );
        }
    }

    #[test]
    fn broker_session_create_errors_classify_for_retry_policy() {
        for status in [0u16, 408, 429, 500, 503] {
            assert_eq!(
                classify_broker_session_create_error(status),
                BrokerErrorCategory::Transient,
                "status {status}"
            );
        }
        assert_eq!(
            classify_broker_session_create_error(409),
            BrokerErrorCategory::Conflict
        );
        for status in [400u16, 401, 403, 404, 422] {
            assert_eq!(
                classify_broker_session_create_error(status),
                BrokerErrorCategory::Terminal,
                "status {status}"
            );
        }
    }

    #[test]
    fn acquire_http_and_typed_error_statuses_match_upstream() {
        let typed_not_found =
            upstream_run_service_error_body("actions-run-service", 404, "gone").to_string();
        let typed_already_acquired =
            upstream_run_service_error_body("actions-run-service", 409, "already acquired")
                .to_string();
        let typed_unprocessable =
            upstream_run_service_error_body("actions-run-service", 422, "unprocessable")
                .to_string();
        let typed_other =
            upstream_run_service_error_body("actions-run-service", 503, "retry later").to_string();

        // HTTP status gates success. A typed error envelope in a 2xx response
        // is not interpreted as an error by RunServiceHttpClient.
        assert_eq!(
            classify_acquire_job_response(200, &typed_not_found),
            AcquireJobResponseClass::Success
        );
        // Once the outer HTTP response fails, only the three typed upstream
        // error codes stop RunServer's retry loop, even when the outer status
        // differs from the body's statusCode.
        assert_eq!(
            classify_acquire_job_response(500, &typed_not_found),
            AcquireJobResponseClass::Skipped(AcquireJobSkipReason::NotFound)
        );
        assert_eq!(
            classify_acquire_job_response(503, &typed_already_acquired),
            AcquireJobResponseClass::Skipped(AcquireJobSkipReason::AlreadyAcquired)
        );
        assert_eq!(
            classify_acquire_job_response(400, &typed_unprocessable),
            AcquireJobResponseClass::Skipped(AcquireJobSkipReason::Unprocessable)
        );
        // A typed but otherwise unrecognized response code follows upstream's
        // generic exception path and retries, regardless of outer 4xx/5xx.
        assert_eq!(
            classify_acquire_job_response(404, &typed_other),
            AcquireJobResponseClass::RetryableFailure
        );
        assert_eq!(
            classify_acquire_job_response(401, "not a run-service error"),
            AcquireJobResponseClass::RetryableFailure
        );
        assert_eq!(
            classify_acquire_job_response(
                404,
                &upstream_run_service_error_body("actions-broker", 404, "gone").to_string()
            ),
            AcquireJobResponseClass::RetryableFailure
        );
        // A body cannot manufacture an HTTP response when the transport has
        // no status.
        assert_eq!(
            classify_acquire_job_response(0, &typed_not_found),
            AcquireJobResponseClass::RetryableFailure
        );
    }

    #[test]
    fn renew_failure_is_job_gone_only_for_typed_404() {
        let gone = github_api_error(
            "renew run-service job",
            404,
            upstream_run_service_error_body("actions-run-service", 404, "Job not found")
                .to_string(),
        );
        assert!(renew_failure_is_job_gone(&gone));
        // The typed body decides, not the envelope status.
        let gone_behind_5xx = github_api_error(
            "renew run-service job",
            500,
            upstream_run_service_error_body("actions-run-service", 404, "Job not found")
                .to_string(),
        );
        assert!(renew_failure_is_job_gone(&gone_behind_5xx));
        let server_error = github_api_error(
            "renew run-service job",
            500,
            upstream_run_service_error_body("actions-run-service", 500, "server error").to_string(),
        );
        assert!(!renew_failure_is_job_gone(&server_error));
        let untyped = github_api_error("renew run-service job", 404, r#"{"message":"Not Found"}"#);
        assert!(!renew_failure_is_job_gone(&untyped));
        let unauthorized = github_api_error("renew run-service job", 401, "");
        assert!(!renew_failure_is_job_gone(&unauthorized));
        // A transport failure never proves anything about ownership.
        assert!(!renew_failure_is_job_gone(&anyhow::anyhow!(
            "connection reset"
        )));
    }

    #[test]
    fn completion_response_classifies_terminal_observations_without_retry() {
        assert_eq!(
            classify_completion_response(204, ""),
            CompletionResponseClass::Accepted
        );
        assert_eq!(
            classify_completion_response(
                404,
                &upstream_run_service_error_body("actions-run-service", 404, "Job not found")
                    .to_string(),
            ),
            CompletionResponseClass::RemoteObservedTerminal
        );
        assert_eq!(
            classify_completion_response(
                500,
                &upstream_run_service_error_body("actions-run-service", 404, "Job not found")
                    .to_string(),
            ),
            CompletionResponseClass::RemoteObservedTerminal
        );
        assert_eq!(
            classify_completion_response(
                404,
                &upstream_run_service_error_body("actions-run-service", 500, "server error")
                    .to_string(),
            ),
            CompletionResponseClass::PermanentFailure
        );
        assert_eq!(
            classify_completion_response(
                204,
                &upstream_run_service_error_body("actions-run-service", 404, "Job not found")
                    .to_string(),
            ),
            CompletionResponseClass::Accepted
        );
        // An unrelated service emitting the same envelope is not proof that
        // this job is terminal: upstream gates on `source` too.
        assert_eq!(
            classify_completion_response(
                404,
                &upstream_run_service_error_body("actions-broker", 404, "Job not found")
                    .to_string(),
            ),
            CompletionResponseClass::PermanentFailure
        );
        assert_eq!(
            classify_completion_response(409, ""),
            CompletionResponseClass::RetryableFailure
        );
        assert_eq!(
            classify_completion_response(404, r#"{"message":"Not Found"}"#),
            CompletionResponseClass::PermanentFailure
        );
        assert_eq!(
            classify_completion_response(503, ""),
            CompletionResponseClass::RetryableFailure
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn complete_job_returns_terminal_ack_without_retrying() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let run_service_url = format!("{}/run/jobs/123", server.uri());
        let completion = RunServiceCompleteJob {
            plan_id: "plan".to_owned(),
            job_id: "job".to_owned(),
            conclusion: TaskResult::Succeeded,
            outputs: BTreeMap::new(),
            step_results: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            environment_url: None,
            billing_owner_id: None,
            infrastructure_failure_category: None,
        };

        for status in [404] {
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("/run/jobs/123/completejob"))
                .respond_with(ResponseTemplate::new(status).set_body_json(
                    upstream_run_service_error_body("actions-run-service", status, "Job not found"),
                ))
                .expect(1)
                .mount(&server)
                .await;

            let acknowledgement = RunServiceClient::new("token")
                .unwrap()
                .complete_job_with_acknowledgement(&run_service_url, completion.clone())
                .await
                .unwrap();
            assert_eq!(
                acknowledgement,
                CompletionAcknowledgement::RemoteObservedTerminal
            );
        }
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn complete_job_terminal_refusal_carries_boundary_category() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let run_service_url = format!("{}/run/jobs/123", server.uri());
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/completejob"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid payload"))
            .expect(1)
            .mount(&server)
            .await;

        let completion = RunServiceCompleteJob {
            plan_id: "plan".to_owned(),
            job_id: "job".to_owned(),
            conclusion: TaskResult::Succeeded,
            outputs: BTreeMap::new(),
            step_results: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            environment_url: None,
            billing_owner_id: None,
            infrastructure_failure_category: None,
        };
        let error = RunServiceClient::new("token")
            .unwrap()
            .complete_job(&run_service_url, completion)
            .await
            .expect_err("a 400 completion must surface the terminal refusal");

        assert_eq!(
            broker_error_category(&error),
            Some(BrokerErrorCategory::Terminal)
        );
        assert!(completion_failure_is_permanent(&error));
        // Message-compatible: the category rides on the typed error, so the
        // historical boundary message is unchanged.
        assert_eq!(
            error.to_string(),
            "complete run-service job failed: status=400, body=invalid payload"
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn complete_job_exhausted_transient_carries_boundary_category() {
        // Regression (r0-798-corr): before the correction the exhausted
        // transient budget surfaced an untyped error (`broker_error_category`
        // returned `None` and permanence fell back to status re-derivation).
        // The loop now decides on the category and the produced error carries
        // it, so the journal reads the boundary verdict in both outcomes.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let run_service_url = format!("{}/run/jobs/123", server.uri());
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/completejob"))
            .respond_with(ResponseTemplate::new(503).set_body_string("try later"))
            .expect(6)
            .mount(&server)
            .await;

        let completion = RunServiceCompleteJob {
            plan_id: "plan".to_owned(),
            job_id: "job".to_owned(),
            conclusion: TaskResult::Succeeded,
            outputs: BTreeMap::new(),
            step_results: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            environment_url: None,
            billing_owner_id: None,
            infrastructure_failure_category: None,
        };
        let error = RunServiceClient::new("token")
            .unwrap()
            .with_complete_retry_delay_for_test(Duration::ZERO)
            .complete_job(&run_service_url, completion)
            .await
            .expect_err("an always-503 completion must exhaust the retry budget");

        assert_eq!(
            broker_error_category(&error),
            Some(BrokerErrorCategory::Transient)
        );
        assert!(!completion_failure_is_permanent(&error));
        assert_eq!(
            error.to_string(),
            "complete run-service job failed: status=503, body=try later"
        );
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

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn create_session_failures_carry_boundary_category() {
        // Regression (r0-798-corr): the session-create producer was untyped,
        // so the retry loop could not read a category at all. A deterministic
        // refusal now carries `Terminal` (fail fast), a transport-shaped
        // failure `Transient`, and a 409 `Conflict`.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let broker = BrokerClient::new(&server.uri(), "token").unwrap();
        let session = TaskAgentSession::new("owner (PID: 1)", 42, "velnor-test");

        for (status, body, expected) in [
            (401u16, "bad credentials", BrokerErrorCategory::Terminal),
            (503u16, "try later", BrokerErrorCategory::Transient),
            (409u16, "already exists", BrokerErrorCategory::Conflict),
        ] {
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("/session"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .expect(1)
                .mount(&server)
                .await;

            let error = broker
                .create_session(&session)
                .await
                .expect_err("a failed session create must surface the refusal");
            assert_eq!(
                broker_error_category(&error),
                Some(expected),
                "status {status}"
            );
            assert_eq!(
                error.to_string(),
                format!("create broker session failed: status={status}, body={body}"),
                "status {status}"
            );
        }
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

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_retries_transient_failure_before_parsing_job() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::{matchers::method, Mock, MockServer, Request, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/run/jobs/123/acquirejob"))
            .respond_with(move |_request: &Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(500).set_body_string("retry later")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "plan": {"planId": "00000000-0000-0000-0000-000000000002"},
                        "jobId": "00000000-0000-0000-0000-000000000001"
                    }))
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
        assert_eq!(job.raw["jobId"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_returns_typed_default_for_malformed_success_without_retry() {
        use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("not-json"),
            )
            .expect(1)
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
            .expect("JsonReaderException maps to the typed reference default");

        let AcquireJobOutcome::Acquired(value) = outcome else {
            panic!("a successful malformed JSON body remains a successful typed response");
        };
        assert!(value.raw.is_null());
        assert!(value.message.is_none());
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_retries_non_object_json_typed_decode_failure() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::{
            matchers::method, matchers::path, Mock, MockServer, Request, ResponseTemplate,
        };

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(move |_request: &Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "application/json")
                        .set_body_string("[]")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "planId": "00000000-0000-0000-0000-000000000002",
                        "jobId": "00000000-0000-0000-0000-000000000001"
                    }))
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
            .expect("generic typed deserialization errors retry");

        let AcquireJobOutcome::Acquired(value) = outcome else {
            panic!("the valid second acquire response must be returned");
        };
        assert_eq!(value.raw["jobId"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_retries_nested_typed_decode_failure() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::{
            matchers::method, matchers::path, Mock, MockServer, Request, ResponseTemplate,
        };

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(move |_request: &Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "application/json")
                        .set_body_json(json!({
                            "resources": {"endpoints": [{"groupScopeId": null}]}
                        }))
                } else {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "application/json")
                        .set_body_json(json!({
                            "jobId": "00000000-0000-0000-0000-000000000001"
                        }))
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
            .expect("nested typed deserialization errors retry");

        let AcquireJobOutcome::Acquired(value) = outcome else {
            panic!("the valid second acquire response must be returned");
        };
        assert_eq!(value.raw["jobId"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_retries_clr_guid_workspace_and_nested_typed_failures() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::{
            matchers::method, matchers::path, Mock, MockServer, Request, ResponseTemplate,
        };

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(move |_request: &Request| {
                match responder_attempts.fetch_add(1, Ordering::SeqCst) {
                    0 => ResponseTemplate::new(200).set_body_json(json!({"jobId": "not-a-guid"})),
                    1 => ResponseTemplate::new(200).set_body_json(json!({
                        "jobId": "00000000-0000-0000-0000-000000000001",
                        "workspace": []
                    })),
                    2 => ResponseTemplate::new(200).set_body_json(json!({
                        "resources": {"endpoints": [{"id": []}]}
                    })),
                    _ => ResponseTemplate::new(200).set_body_json(json!({
                        "jobId": "00000000-0000-0000-0000-000000000001",
                        "plan": {"planId": "00000000-0000-0000-0000-000000000002"}
                    })),
                }
            })
            .expect(4)
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
            .expect("CLR GUID, workspace, and endpoint type failures retry");

        let AcquireJobOutcome::Acquired(value) = outcome else {
            panic!("the valid fourth response must be returned");
        };
        assert_eq!(value.raw["jobId"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_keeps_nullable_clr_wire_message_until_runtime_admission() {
        use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "messageType": "PipelineAgentJobRequest",
                "plan": {"planId": "00000000-0000-0000-0000-000000000002"},
                "timeline": {"id": "00000000-0000-0000-0000-000000000003"},
                "jobId": "00000000-0000-0000-0000-000000000001",
                "jobDisplayName": null,
                "variables": null,
                "mask": null,
                "steps": null,
                "resources": {"endpoints": null, "repositories": null, "containers": null},
                "environmentVariables": null,
                "defaults": null
            })))
            .expect(1)
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
            .expect("partial but syntactically valid DTO is a successful response");

        let AcquireJobOutcome::Acquired(value) = outcome else {
            panic!("partial job object must not become a retryable response error");
        };
        let message = value
            .message
            .expect("validated wire model is retained before runtime admission");
        assert_eq!(
            message.message_type.as_deref(),
            Some("PipelineAgentJobRequest")
        );
        assert_eq!(message.job_id, "00000000-0000-0000-0000-000000000001");
        assert!(message.job_display_name.is_none());
        assert_eq!(message.request_id, 0);
        assert!(message.variables.is_empty());
        assert!(message.mask.is_empty());
        assert!(message.steps.is_empty());
        assert!(message.resources.as_ref().unwrap().endpoints.is_empty());
        assert!(message.environment_variables.is_empty());
        assert!(message.defaults.is_empty());
        assert!(message.actions_dependencies.is_empty());
    }

    #[test]
    fn converter_discriminators_match_newtonsoft_exact_priority_and_selected_subtypes() {
        let body = r#"{
            "jobContainer":{"Type":1,"type":0,"lit":"job-token","seq":17},
            "contextData":{
                "missing":{"Noise":true},
                "collision":{"T":1,"t":0,"s":"selected","a":[{"t":99}]},
                "invalid":{"t":"not-an-integer","a":[{"t":99}]}
            },
            "steps":[{
                "type":5,"Type":4,"StepIds":17,
                "Reference":{"type":2,"Type":1,"Name":"owner/action","Image":[]}
            }]
        }"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap();
        assert_eq!(payload.raw_json, body);
        let message = payload.message.expect("CLR-valid wire DTO is retained");

        let container = message.job_container.as_ref().unwrap();
        assert_eq!(container["type"], 0);
        assert_eq!(container["lit"], "job-token");
        assert!(container.get("seq").is_none());

        let context = message.context_data.as_ref().unwrap();
        assert_eq!(context["missing"]["t"], 0);
        assert_eq!(context["missing"].as_object().unwrap().len(), 1);
        assert_eq!(context["collision"]["t"], 0);
        assert_eq!(context["collision"]["s"], "selected");
        assert!(context["collision"].get("a").is_none());
        assert_eq!(context["invalid"], Value::Null);
        assert_eq!(message.materialize_context_data().unwrap()["missing"], "");
        assert_eq!(
            message.materialize_context_data().unwrap()["collision"],
            "selected"
        );

        let step = message.steps[0].as_ref().unwrap();
        assert_eq!(
            step.step_kind(),
            Some(crate::job_message::ActionStepKind::Action)
        );
        let reference = step.reference.as_ref().unwrap();
        assert_eq!(
            reference.r#type,
            Some(crate::job_message::ActionReferenceType::Repository)
        );
        assert_eq!(reference.name.as_deref(), Some("owner/action"));
        assert!(reference.image.is_none());
        assert_eq!(step.step_ids, Vec::<Option<String>>::new());
    }

    #[test]
    fn context_map_duplicates_reset_converter_existing_value_and_step_tokens_retain_it() {
        let body = r#"{"contextData":{"x":"kept","x":{"t":"invalid"}},"steps":[{"Type":4,"Environment":{"type":2},"environment":{"type":"invalid"}}]}"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap();
        let message = payload.message.unwrap();

        assert_eq!(message.context_data.as_ref().unwrap()["x"], Value::Null);
        assert_eq!(
            message.steps[0].as_ref().unwrap().environment,
            Some(json!({ "type": 2 }))
        );
    }

    #[test]
    fn acquire_context_data_requires_object_root_before_ordered_pair_projection() {
        let array_root = r#"{"contextData":[["root",{"t":0,"s":"value"}]]}"#;
        assert!(
            decode_acquire_job_success_body(
                200,
                Some(array_root.len() as u64),
                Some("application/json"),
                array_root,
            )
            .is_err(),
            "raw arrays cannot masquerade as internal ordered ContextData pairs"
        );

        let undefined_root = r#"{"contextData":undefined}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(undefined_root.len() as u64),
            Some("application/json"),
            undefined_root,
        )
        .is_err());

        let constructor_root = r#"{"contextData":new Foo({"root":{"t":0,"s":"value"}})}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(constructor_root.len() as u64),
            Some("application/json"),
            constructor_root,
        )
        .is_err());

        let null_root = r#"{"contextData":null}"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(null_root.len() as u64),
            Some("application/json"),
            null_root,
        )
        .unwrap();
        assert!(payload.message.unwrap().context_data.is_none());
    }

    #[test]
    fn acquired_nonfinite_scalars_and_converter_overflow_keep_clr_behavior() {
        let body = r#"{"jobContainer":NaN,"contextData":{"bare":NaN}}"#;
        let message = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap()
        .message
        .unwrap();
        assert_eq!(
            crate::job_message::template_token_context_value(
                message.job_container.as_ref().unwrap()
            )
            .unwrap(),
            velnor_model::ContextValue::non_finite(velnor_model::NonFinite::NaN)
        );
        assert_eq!(
            message.materialize_context_values().unwrap()["bare"],
            velnor_model::ContextValue::non_finite(velnor_model::NonFinite::NaN)
        );

        for body in [
            r#"{"jobContainer":{"type":2147483648}}"#,
            r#"{"contextData":{"x":{"t":2147483648}}}"#,
            r#"{"jobContainer":9223372036854775808}"#,
            r#"{"contextData":{"x":9223372036854775808}}"#,
            r#"{"jobContainer":18446744073709551616}"#,
            r#"{"contextData":{"x":18446744073709551616}}"#,
            r#"{"jobContainer":{"type":6,"num":null,"Num":1}}"#,
            r#"{"contextData":{"x":{"t":4,"n":null,"N":1}}}"#,
            r#"{"jobContainer":{"type":6,"Num":null,"num":1}}"#,
            r#"{"contextData":{"x":{"t":4,"N":null,"n":1}}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body,
                )
                .is_err(),
                "{body}"
            );
        }

        let action_reference_alias =
            r#"{"steps":[{"Type":4,"Reference":{"Type":1,"name":[],"Name":"valid"}}]}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(action_reference_alias.len() as u64),
            Some("application/json"),
            action_reference_alias,
        )
        .expect("bad earlier ActionReference alias follows the reader-null boundary");
        assert!(decoded.message.is_none());
    }

    #[test]
    fn acquired_double_materialization_rounds_int64_and_preserves_raw_json() {
        let body = r#"{"jobId":"00000000-0000-0000-0000-000000000001","plan":{"planId":"00000000-0000-0000-0000-000000000002"},"timeline":{"id":"00000000-0000-0000-0000-000000000003"},"jobContainer":{"type":6,"num":9007199254740993},"contextData":{"number":{"t":4,"n":9007199254740993},"root_integer":9007199254740993,"array_root":[1],"unknown-field":{"t":4,"n":1,"b":[]}}}"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap();

        assert_eq!(payload.raw_json, body);
        assert_eq!(
            payload.raw["contextData"]["number"]["n"],
            9007199254740993_u64
        );
        let message = payload.message.unwrap();
        let runtime = message.materialize_runtime().unwrap();
        let token_number = runtime.job_container.as_ref().unwrap()["num"]
            .as_number()
            .unwrap();
        assert!(token_number.is_f64());
        assert_eq!(token_number.as_f64(), Some(9007199254740992.0));
        let contexts = runtime.materialize_context_data().unwrap();
        let context_number = contexts["number"].as_number().unwrap();
        assert!(context_number.is_f64());
        assert_eq!(context_number.as_f64(), Some(9007199254740992.0));
        let root_integer = contexts["root_integer"].as_number().unwrap();
        assert!(root_integer.is_f64());
        assert_eq!(root_integer.as_f64(), Some(9007199254740992.0));
        assert_eq!(contexts["array_root"], Value::Null);
        assert_eq!(contexts["unknown-field"], 1.0);

        for (integer, expected_bits) in [
            ("9223372036854776833", 0x43e0_0000_0000_0000),
            ("18446744073709553665", 0x43f0_0000_0000_0000),
        ] {
            let big_integer_double = format!(r#"{{"jobContainer":{{"type":6,"num":{integer}}}}}"#);
            let payload = decode_acquire_job_success_body(
                200,
                Some(big_integer_double.len() as u64),
                Some("application/json"),
                &big_integer_double,
            )
            .expect("Newtonsoft BigInteger to Double cast succeeds");
            let runtime = payload
                .message
                .expect("typed Double materializes")
                .materialize_runtime()
                .unwrap();
            assert_eq!(
                runtime.job_container.as_ref().unwrap()["num"]
                    .as_f64()
                    .unwrap()
                    .to_bits(),
                expected_bits,
                "CLR truncating BigInteger cast: {integer}"
            );
        }

        let big_integer_context = r#"{"contextData":{"x":9223372036854775808}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(big_integer_context.len() as u64),
            Some("application/json"),
            big_integer_context,
        )
        .is_err());

        for body in [
            r#"{"contextData":{"number":{"t":4,"n":null}}}"#,
            r#"{"jobContainer":{"type":6,"num":null}}"#,
        ] {
            assert!(decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .is_err());
        }
    }

    #[test]
    fn acquired_converters_collapse_exact_jobject_duplicates_and_keep_existing_tokens() {
        let body = r#"{"jobId":"00000000-0000-0000-0000-000000000001","plan":{"planId":"00000000-0000-0000-0000-000000000002"},"JobServiceContainers":{"type":2,"map":[]},"JobServiceContainers":{"type":"invalid"},"JobSidecarContainers":{"network":"sidecar"},"steps":[{"Type":4,"Enabled":null,"Enabled":true}]}"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap();
        assert_eq!(payload.raw_json, body);
        let message = payload.message.expect("JObject converter cases are valid");

        let services = message.job_service_containers.as_ref().unwrap();
        assert_eq!(services["type"], 2);
        assert_eq!(services["map"], json!([]));
        assert!(message.steps[0].as_ref().unwrap().enabled);
    }

    #[test]
    fn acquired_converter_collection_aliases_append_and_null_resets_lists() {
        let body = r#"{"jobId":"00000000-0000-0000-0000-000000000001","plan":{"planId":"00000000-0000-0000-0000-000000000002"},"JobContainer":{"type":1,"seq":[{"type":0,"lit":"a"}],"Seq":[{"type":0,"lit":"b"}]},"JobServiceContainers":{"type":2,"map":[{"key":{"type":0,"lit":"ka"},"value":{"type":0,"lit":"va"}}],"Map":[{"key":{"type":0,"lit":"kb"},"value":{"type":0,"lit":"vb"}}]},"contextData":{"array":{"t":1,"a":[{"t":0,"s":"a"}],"A":[{"t":0,"s":"b"}]},"dictionary":{"t":2,"d":[{"k":"a","v":{"t":0,"s":"a"}}],"D":[{"k":"b","v":{"t":0,"s":"b"}}]},"caseSensitive":{"t":5,"d":[{"k":"a","v":{"t":0,"s":"a"}}],"D":[{"k":"b","v":{"t":0,"s":"b"}}]},"reset":{"t":1,"a":[{"t":0,"s":"discard"}],"A":null},"duplicate":{"t":1,"a":[{"t":0,"s":"discard"}],"a":[{"t":0,"s":"kept"}],"A":[{"t":0,"s":"appended"}]}}}"#;
        let payload = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .unwrap();

        assert_eq!(payload.raw_json, body);
        let message = payload.message.unwrap();
        assert_eq!(
            message.job_container.as_ref().unwrap()["seq"][0]["lit"],
            "a"
        );
        assert_eq!(
            message.job_container.as_ref().unwrap()["seq"][1]["lit"],
            "b"
        );
        assert_eq!(
            message.job_service_containers.as_ref().unwrap()["map"][0]["value"]["lit"],
            "va"
        );
        assert_eq!(
            message.job_service_containers.as_ref().unwrap()["map"][1]["value"]["lit"],
            "vb"
        );
        let context = message.context_data.unwrap();
        assert_eq!(context["array"]["a"][0]["s"], "a");
        assert_eq!(context["array"]["a"][1]["s"], "b");
        assert_eq!(context["dictionary"]["d"].as_array().unwrap().len(), 2);
        assert_eq!(context["caseSensitive"]["d"].as_array().unwrap().len(), 2);
        assert!(
            context["reset"]["a"].is_null(),
            "a later null alias resets the list"
        );
        assert_eq!(context["duplicate"]["a"][0]["s"], "kept");
        assert_eq!(context["duplicate"]["a"][1]["s"], "appended");
        assert_eq!(context["duplicate"]["a"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn acquire_success_body_decoding_matches_typed_json_defaults() {
        let valid_partial = r#"{"jobId":"00000000-0000-0000-0000-000000000001","plan":{"planId":"00000000-0000-0000-0000-000000000002"}}"#;
        let assert_reader_default = |body: &str| {
            let payload = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .unwrap();
            assert_eq!(payload.raw, serde_json::from_str::<Value>(body).unwrap());
            assert!(payload.message.is_none());
        };
        let decoded = decode_acquire_job_success_body(
            200,
            Some(valid_partial.len() as u64),
            Some("application/json; charset=utf-8"),
            valid_partial,
        )
        .unwrap();
        assert_eq!(
            decoded.raw,
            serde_json::json!({
                "jobId":"00000000-0000-0000-0000-000000000001",
                "plan":{"planId":"00000000-0000-0000-0000-000000000002"}
            })
        );
        assert!(decoded.message.is_some());
        for (status, length, content_type, body) in [
            (204, Some(0), Some("application/json"), ""),
            (200, Some(0), Some("application/json"), ""),
            (200, Some(8), Some("text/plain"), "not-json"),
            (200, Some(8), Some("application/json"), "not-json"),
            (200, Some(4), Some("application/json"), "null"),
        ] {
            let decoded =
                decode_acquire_job_success_body(status, length, content_type, body).unwrap();
            assert!(decoded.raw.is_null());
            assert!(decoded.message.is_none());
            assert_eq!(decoded.identity, AcquiredJobIdentity::default());
        }
        let typed_error_envelope = upstream_run_service_error_body(
            "actions-run-service",
            404,
            "not an error when HTTP succeeded",
        )
        .to_string();
        let decoded = decode_acquire_job_success_body(
            200,
            Some(typed_error_envelope.len() as u64),
            Some("application/json"),
            &typed_error_envelope,
        )
        .expect("2xx HTTP wins before typed error-body parsing");
        assert!(decoded.message.is_some());
        assert_eq!(decoded.identity, AcquiredJobIdentity::default());
        assert!(
            decode_acquire_job_success_body(200, Some(2), Some("application/json"), "[]").is_err()
        );
        assert!(decode_acquire_job_success_body(
            200,
            Some(12),
            Some("application/json"),
            r#"{"jobId":"not-a-guid"}"#
        )
        .is_err());
        assert!(decode_acquire_job_success_body(
            200,
            Some(14),
            Some("application/json"),
            r#"{"workspace":[]}"#
        )
        .is_err());
        assert!(decode_acquire_job_success_body(
            200,
            Some(19),
            Some("application/json"),
            r#"{"lockedUntil":null}"#
        )
        .is_err());
        assert!(decode_acquire_job_success_body(
            200,
            Some(19),
            Some("application/json"),
            r#"{"requestId":null}"#
        )
        .is_err());
        assert!(decode_acquire_job_success_body(
            200,
            Some(22),
            Some("application/json"),
            r#"{"enableDebugger":null}"#
        )
        .is_err());
        assert_reader_default(r#"{"lockedUntil":"not-a-date"}"#);
        for body in [
            r#"{"enableDebugger":"not-bool"}"#,
            r#"{"lockedUntil":123}"#,
            r#"{"lockedUntil":true}"#,
            r#"{"lockedUntil":[]}"#,
            r#"{"lockedUntil":"not-a-date"}"#,
            r#"{"plan":{"version":"not-int"}}"#,
            r#"{"jobDisplayName":[]}"#,
            r#"{"variables":{"x":{"value":[]}}}"#,
            r#"{"steps":[{"type":"Action","name":[] }]}"#,
            r#"{"jobContainer":{"type":0,"lit":[]}}"#,
            r#"{"steps":[{"type":"Action","reference":{"type":"Repository","name":[]}}]}"#,
        ] {
            assert_reader_default(body);
        }
        for body in [
            r#"{"plan":{"version":undefined}}"#,
            r#"{"requestId":undefined}"#,
            r#"{"enableDebugger":undefined}"#,
            r#"{"lockedUntil":undefined}"#,
            r#"{"contextData":{"x":{"t":4,"n":undefined}}}"#,
        ] {
            let payload = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .expect("JsonReaderException maps to null-success");
            assert!(
                payload.message.is_none(),
                "undefined typed primitive must produce reader-null disposition: {body}"
            );
        }
        let forged_reader_text = r#"{"plan":"CLR_JSON_READER_ERROR:"}"#;
        assert!(
            decode_acquire_job_success_body(
                200,
                Some(forged_reader_text.len() as u64),
                Some("application/json"),
                forged_reader_text
            )
            .is_err(),
            "payload text cannot forge the typed JsonReaderException category"
        );
        assert!(decode_acquire_job_success_body(
            200,
            Some(18),
            Some("application/json"),
            r#"{"lockedUntil":""}"#
        )
        .is_err());
        assert!(decode_acquire_job_success_body(
            200,
            Some(17),
            Some("application/json"),
            r#"{"plan":{"version":""}}"#
        )
        .is_err());
        let assert_typed_datetime = |body: &str| {
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .unwrap();
            assert!(decoded.message.is_some(), "valid CLR DateTime: {body}");
        };
        assert_typed_datetime(r#"{"lockedUntil":"2026-10-04"}"#);
        assert_typed_datetime(r#"{"lockedUntil":"October 4, 2026"}"#);
        assert_typed_datetime(r#"{"lockedUntil":"01/02/2003"}"#);
        assert_typed_datetime(r#"{"lockedUntil":"01/02/2003 3:04:05 PM"}"#);
        for date in ["March 4, 2025", "3/4/2025", "March 4, 2025 5:06 PM"] {
            let body = format!(r#"{{"lockedUntil":"{date}"}}"#);
            assert_typed_datetime(&body);
        }
        assert_typed_datetime(r#"{"lockedUntil":"/Date(-1)/"}"#);
        for reader_null_date in [
            r#"{"lockedUntil":"/Date(+1)/"}"#,
            r#"{"lockedUntil":"/Date(1++1)/"}"#,
            r#"{"lockedUntil":"/Date(1+010)/"}"#,
        ] {
            assert_reader_default(reader_null_date);
        }
        assert_typed_datetime(r#"{"lockedUntil":"/Date(1844674407370955)/"}"#);
        let out_of_range_microsoft_date = r#"{"lockedUntil":"/Date(1000000000000000)/"}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(out_of_range_microsoft_date.len() as u64),
            Some("application/json"),
            out_of_range_microsoft_date
        )
        .is_err());
        assert_typed_datetime(r#"{"lockedUntil":"10/4/2026 13:30:00"}"#);
        for valid_dotnet_datetime in [
            r#"{"lockedUntil":"2025-03-04T05:06"}"#,
            r#"{"lockedUntil":"2025-03-04 05:06:07"}"#,
        ] {
            assert_typed_datetime(valid_dotnet_datetime);
        }
        let malformed_microsoft_offset = r#"{"lockedUntil":"/Date(0+bogus)/"}"#;
        assert_reader_default(malformed_microsoft_offset);

        let string_boolean = r#"{"enableDebugger":"true"}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(string_boolean.len() as u64),
            Some("application/json"),
            string_boolean
        )
        .is_ok());
        assert_reader_default(r#"{"enableDebugger":"nonsense"}"#);
        assert!(decode_acquire_job_success_body(
            200,
            Some(20),
            Some("application/json"),
            r#"{"enableDebugger":""}"#
        )
        .is_err());

        for guid in [
            "(00000000-0000-0000-0000-000000000001)",
            "{0x00000000,0x0000,0x0000,{0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x01}}",
        ] {
            let body = json!({"jobId": guid}).to_string();
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    &body
                )
                .is_ok(),
                "CLR Guid parser accepts X and parenthesized forms: {guid}"
            );
        }
        let urn_guid = r#"{"jobId":"urn:uuid:00000000-0000-0000-0000-000000000001"}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(urn_guid.len() as u64),
            Some("application/json"),
            urn_guid
        )
        .is_err());

        for body in [r#"{"mask":[{"type":1.0}]}"#, r#"{"mask":[{"type":1.5}]}"#] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "StringEnumConverter rejects floating MaskType: {body}"
            );
        }
        let numeric_and_combined_mask_types = json!({
            "mask": [{"type": 4294967297u64}, {"type": "Variable, Regex"}]
        })
        .to_string();
        assert!(decode_acquire_job_success_body(
            200,
            Some(numeric_and_combined_mask_types.len() as u64),
            Some("application/json"),
            &numeric_and_combined_mask_types
        )
        .is_ok());

        let null_context_pair = r#"{"contextData":{"x":{"t":2,"d":[null]}}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(null_context_pair.len() as u64),
            Some("application/json"),
            null_context_pair
        )
        .is_ok());

        for value in [
            json!({"jobContainer": Value::from(u64::MAX)}),
            json!({"contextData": {"x": Value::from(u64::MAX)}}),
        ] {
            let body = value.to_string();
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    &body
                )
                .is_err(),
                "dynamic integer overflow must retry: {body}"
            );
        }

        for body in [
            r#"{"requestId":"not-int"}"#,
            r#"{"requestId":[]}"#,
            r#"{"debuggerTunnel":{"port":"not-int"}}"#,
            r#"{"debuggerTunnel":{"port":65536}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "JsonSerializationException follows the acquire retry path: {body}"
            );
        }

        let coerced: ClrAgentJobRequestMessage = serde_json::from_value(json!({
            "enableDebugger": 1,
            "plan": {"version": 1.5},
            "requestId": "1",
            "debuggerTunnel": {"port": "1"},
            "jobDisplayName": true
        }))
        .expect("Json.NET typed readers coerce primitive tokens");
        assert!(coerced.enable_debugger);
        assert_eq!(coerced.plan.unwrap().version.0, 2);
        assert_eq!(coerced.request_id.0, 1);
        assert_eq!(coerced.debugger_tunnel.unwrap().port, 1);
        assert_eq!(coerced.job_display_name.as_deref(), Some("True"));

        for body in [
            r#"{"jobContainer":{"type":999}}"#,
            r#"{"contextData":{"github":{"t":999}}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "unknown converter discriminator must remain retryable: {body}"
            );
        }
        for body in [
            r#"{"steps":[{"Type":4,"StepIds":123}]}"#,
            r#"{"steps":[{"Type":5,"Background":[]}]}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_ok(),
                "fields absent from this concrete Step variant are ignored: {body}"
            );
        }
        let uppercase_bad_guid = r#"{"JOBID":"not-a-guid"}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(uppercase_bad_guid.len() as u64),
            Some("application/json"),
            uppercase_bad_guid
        )
        .is_err());
        let uppercase_valid_guid = r#"{"JOBID":"00000000-0000-0000-0000-000000000001"}"#;
        let uppercase_payload = decode_acquire_job_success_body(
            200,
            Some(uppercase_valid_guid.len() as u64),
            Some("application/json"),
            uppercase_valid_guid,
        )
        .unwrap();
        assert_eq!(
            uppercase_payload.raw,
            json!({"JOBID":"00000000-0000-0000-0000-000000000001"})
        );
        assert!(uppercase_payload.message.is_some());
        let whitespace_braced_guid = r#"{"jobId":" {00000000-0000-0000-0000-000000000001} "}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(whitespace_braced_guid.len() as u64),
            Some("application/json"),
            whitespace_braced_guid
        )
        .is_ok());

        for body in [
            r#"{"plan":{"artifactUri":"../artifacts/1"}}"#,
            r#"{"timeline":{"location":"/timeline/1"}}"#,
            r#"{"resources":{"endpoints":[{"url":"https://example.invalid/service"}]}}"#,
            r#"{"plan":{"artifactUri":"http:example.test/a"}}"#,
            r#"{"plan":{"artifactUri":"http:example.com"}}"#,
            r#"{"plan":{"artifactUri":"http:/example.test/a"}}"#,
            r#"{"plan":{"artifactUri":"https:foo"}}"#,
            r#"{"plan":{"artifactUri":"relative path"}}"#,
            r#"{"plan":{"artifactUri":"foo\u0001bar"}}"#,
            r#"{"plan":{"artifactUri":" "}}"#,
            r#"{"resources":{"endpoints":[{"url":"http://example.test/a b"}]}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_ok(),
                "System.Uri accepts relative and absolute references: {body}"
            );
        }
        for body in [
            r#"{"plan":{"artifactUri":"http://["}}"#,
            r#"{"plan":{"artifactLocation":"http://["}}"#,
            r#"{"timeline":{"location":"http://["}}"#,
            r#"{"resources":{"endpoints":[{"url":"http://["}]}}"#,
            r#"{"plan":{"artifactUri":"http:\\\\example.test\\a"}}"#,
            r#"{"plan":{"artifactUri":"http://example.test:99999/"}}"#,
            r#"{"resources":{"endpoints":[{"url":42}]}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "typed System.Uri failures retry: {body}"
            );
        }
        let undefined_uris = r#"{"plan":{"artifactUri":undefined,"artifactLocation":undefined},"timeline":{"location":undefined},"resources":{"endpoints":[{"url":undefined}]}}"#;
        let undefined_uri_payload = decode_acquire_job_success_body(
            200,
            Some(undefined_uris.len() as u64),
            Some("application/json"),
            undefined_uris,
        )
        .expect("Undefined nullable System.Uri fields become null");
        let undefined_uri_message = undefined_uri_payload
            .message
            .expect("nullable URI fields preserve the otherwise valid DTO");
        let plan = undefined_uri_message.plan.unwrap();
        assert!(plan.artifact_uri.is_none());
        assert!(plan.artifact_location.is_none());
        assert!(undefined_uri_message
            .timeline
            .as_ref()
            .unwrap()
            .location
            .is_none());
        assert!(
            undefined_uri_message.resources.as_ref().unwrap().endpoints[0]
                .as_ref()
                .unwrap()
                .url
                .is_none()
        );

        let empty_uri = r#"{"plan":{"artifactUri":"","artifactLocation":""},"timeline":{"location":""},"resources":{"endpoints":[{"url":""}]}}"#;
        let empty_uri_payload = decode_acquire_job_success_body(
            200,
            Some(empty_uri.len() as u64),
            Some("application/json"),
            empty_uri,
        )
        .unwrap();
        let empty_uri_message = empty_uri_payload
            .message
            .expect("empty nullable URI strings become null");
        let plan = empty_uri_message.plan.unwrap();
        assert!(plan.artifact_uri.is_none());
        assert!(plan.artifact_location.is_none());
        assert!(empty_uri_message
            .timeline
            .as_ref()
            .unwrap()
            .location
            .is_none());
        assert!(empty_uri_message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .url
            .is_none());

        let oversized_uri = json!({"timeline": {"location": "a".repeat(65_520)}}).to_string();
        assert!(decode_acquire_job_success_body(
            200,
            Some(oversized_uri.len() as u64),
            Some("application/json"),
            &oversized_uri
        )
        .is_err());
        let too_long_uri = "a".repeat(65_520);
        for body in [
            json!({"plan": {"artifactUri": too_long_uri.clone()}}).to_string(),
            json!({"plan": {"artifactLocation": too_long_uri.clone()}}).to_string(),
            json!({"timeline": {"location": too_long_uri.clone()}}).to_string(),
            json!({"resources": {"endpoints": [{"url": too_long_uri.clone()}]}}).to_string(),
        ] {
            assert!(decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                &body
            )
            .is_err());
        }

        let sidecar_missing_alias =
            r#"{"jobSidecarContainers":{"db":"missing"},"resources":{"containers":[]}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(sidecar_missing_alias.len() as u64),
            Some("application/json"),
            sidecar_missing_alias
        )
        .is_err());
        let max_valid_uri = json!({"timeline": {"location": "a".repeat(65_519)}}).to_string();
        assert!(decode_acquire_job_success_body(
            200,
            Some(max_valid_uri.len() as u64),
            Some("application/json"),
            &max_valid_uri
        )
        .is_ok());

        let expression_name = r#"{"resources":{"repositories":[{"endpoint":{"name":"$[ variables.repo ]"},"properties":{}}]}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(expression_name.len() as u64),
            Some("application/json"),
            expression_name
        )
        .is_ok());
        for body in [
            r#"{"resources":{"repositories":[{"endpoint":{"name":"$[ ]"},"properties":{}}]}}"#,
            r#"{"resources":{"repositories":[{"endpoint":{"name":[]},"properties":{}}]}}"#,
            r#"{"resources":{"repositories":[{"endpoint":{"name":{}},"properties":{}}]}}"#,
            r#"{"plan":{"owner":{"_links":{"self":"not-a-reference-link"}}}}"#,
            r#"{"plan":{"owner":{"_links":{"":{"href":"/"}}}}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "CLR expression and reference-link failures retry: {body}"
            );
        }
        for body in [
            r#"{"resources":{"endpoints":[{"authorization":{"parameters":{"token":[]}}}]}}"#,
            r#"{"resources":{"endpoints":[{"data":{"token":[]}}]}}"#,
            r#"{"resources":{"endpoints":[{"data":{"Token":{},"Token":"last"}}]}}"#,
            r#"{"resources":{"endpoints":[{"authorization":{"parameters":{"Token":[],"token":"last"}}}]}}"#,
            r#"{"jobSidecarContainers":{"db":{},"db":"container"}}"#,
        ] {
            assert_reader_default(body);
        }
        let links_with_null_array_entry =
            r#"{"plan":{"owner":{"_links":{"self":[null,{"href":"/"}]}}}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(links_with_null_array_entry.len() as u64),
            Some("application/json"),
            links_with_null_array_entry
        )
        .is_ok());

        for body in [
            r#"{"jobContainer":{"type":1.0}}"#,
            r#"{"contextData":{"github":{"t":1.0}}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_ok(),
                "the upstream token converters return null for float discriminators: {body}"
            );
        }
        let thousands_double = r#"{"jobContainer":{"type":6,"num":"1,234"}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(thousands_double.len() as u64),
            Some("application/json"),
            thousands_double
        )
        .is_ok());
        let scalar_token_map_key = r#"{"jobContainer":{"type":2,"map":[{"key":{"type":3,"expr":"variables.key"},"value":{"type":0,"lit":"x"}}]}}"#;
        assert!(decode_acquire_job_success_body(
            200,
            Some(scalar_token_map_key.len() as u64),
            Some("application/json"),
            scalar_token_map_key
        )
        .is_ok());
        let token_null_and_converter_fallbacks = json!({
            "jobContainer": null,
            "jobServiceContainers": [],
            "contextData": {"null": null, "array": []}
        });
        let token_body = token_null_and_converter_fallbacks.to_string();
        let decoded = decode_acquire_job_success_body(
            200,
            Some(token_body.len() as u64),
            Some("application/json"),
            &token_body,
        )
        .unwrap();
        assert_eq!(
            decoded.raw, token_null_and_converter_fallbacks,
            "the acquire boundary preserves raw JSON after CLR wire validation"
        );
        assert!(decoded.message.is_some());
        let null_members = r#"{"plan":null,"timeline":null,"resources":null}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(null_members.len() as u64),
            Some("application/json"),
            null_members,
        )
        .unwrap();
        assert!(decoded.raw["plan"].is_null());
        let message = decoded
            .message
            .expect("null reference properties stay on the wire DTO");
        assert_eq!(message.job_id, crate::protocol::EMPTY_LOCK_TOKEN);
        assert!(message.plan.is_none());
        assert!(message.timeline.is_none());
        assert!(message.resources.is_none());
        assert!(message.materialize_runtime().is_err());
    }

    #[test]
    fn acquire_wire_dictionary_comparers_match_pinned_clr_maps() {
        let decode = |body: &str| {
            decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
        };

        let root_maps = r#"{"variables":{"Name":{"value":"one"},"name":{"value":"two"}},"contextData":{"Name":{"t":0,"s":"one"},"name":{"t":0,"s":"two"}}}"#;
        let decoded = decode(root_maps).expect("root dictionaries are ordinal");
        let message = decoded.message.expect("root maps materialize");
        assert_eq!(message.variables.len(), 2);
        assert!(message.variables.contains_key("Name"));
        assert!(message.variables.contains_key("name"));
        let context = message.materialize_context_data().unwrap();
        assert!(context.contains_key("Name"));
        assert!(context.contains_key("name"));

        let endpoint_data = r#"{"resources":{"endpoints":[{"data":{"FeedStreamUrl":"first","feedstreamurl":"last"}}]}}"#;
        let decoded = decode(endpoint_data).expect("endpoint Data is preinitialized CI");
        let message = decoded.message.expect("endpoint Data materializes");
        let data = &message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .data;
        assert_eq!(data.len(), 1);
        assert_eq!(
            data.get("FeedStreamUrl").and_then(Option::as_deref),
            Some("last")
        );

        let unicode_endpoint_data =
            r#"{"resources":{"endpoints":[{"data":{"Å":"first","å":"last"}}]}}"#;
        let decoded = decode(unicode_endpoint_data)
            .expect("OrdinalIgnoreCase endpoint map folds Unicode case variants");
        let message = decoded.message.expect("Unicode endpoint Data materializes");
        let data = &message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .data;
        assert_eq!(data.len(), 1);
        assert_eq!(data.get("Å").and_then(Option::as_deref), Some("last"));

        for collision in [
            r#"{"resources":{"endpoints":[{"authorization":{"parameters":{"Token":"first","token":"last"}}}]}}"#,
            r#"{"resources":{"endpoints":[{"authorization":{"parameters":{"Å":"first","å":"last"}}}]}}"#,
            r#"{"resources":{"repositories":[{"properties":{"Ports":["80"],"ports":["81"]}}]}}"#,
        ] {
            assert!(
                decode(collision).is_err(),
                "CLR copy into OrdinalIgnoreCase dictionary rejects collisions: {collision}"
            );
        }

        let exact_duplicate = r#"{"resources":{"endpoints":[{"authorization":{"parameters":{"Token":"first","Token":"last"}}}]}}"#;
        let decoded = decode(exact_duplicate).expect("exact ordinal duplicate assigns last value");
        let message = decoded.message.expect("parameters materialize");
        let parameters = &message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .authorization
            .as_ref()
            .unwrap()
            .parameters;
        assert_eq!(parameters.len(), 1);
        assert_eq!(
            parameters.get("Token").and_then(Option::as_deref),
            Some("last")
        );
    }

    #[test]
    fn acquire_wire_duplicate_members_follow_clr_population_rules() {
        let decode = |body: &str| {
            decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
        };

        let repeated_plan = r#"{"plan":{"planId":"00000000-0000-0000-0000-000000000002","planType":"first"},"PLAN":{"planGroup":"second"}}"#;
        let message = decode(repeated_plan)
            .unwrap()
            .message
            .expect("repeated Plan objects populate the existing instance");
        let plan = message.plan.unwrap();
        assert_eq!(plan.plan_type.as_deref(), Some("first"));
        assert_eq!(plan.plan_group.as_deref(), Some("second"));

        let repeated_lists = r#"{"mask":[{"type":"Variable","value":"one"}],"MASK":[{"type":"Regex","value":"two"}],"dependencies":["first"],"DEPENDENCIES":["second"]}"#;
        let message = decode(repeated_lists)
            .unwrap()
            .message
            .expect("repeated mutable collections populate their existing lists");
        assert_eq!(message.mask.len(), 2);
        assert_eq!(
            message
                .actions_dependencies
                .iter()
                .map(Option::as_deref)
                .collect::<Vec<_>>(),
            [Some("first"), Some("second")]
        );

        let null_resets_lists = r#"{"mask":[{"type":"Variable","value":"discard"}],"Mask":null,"resources":{"endpoints":[{"name":"discard"}]},"RESOURCES":{"endpoints":null}}"#;
        let message = decode(null_resets_lists)
            .unwrap()
            .message
            .expect("explicit null resets mutable collection fields");
        assert!(message.mask.is_empty());
        assert!(message.resources.unwrap().endpoints.is_empty());

        let replaced_properties =
            r#"{"resources":{"repositories":[{"properties":{"old":1},"PROPERTIES":{"new":2}}]}}"#;
        let message = decode(replaced_properties)
            .unwrap()
            .message
            .expect("ResourceProperties converter replaces existing bags");
        let resources = message.resources.unwrap();
        let repository = resources.repositories[0].as_ref().unwrap();
        assert_eq!(
            repository.properties,
            ContextValue::object(vec![("new".to_owned(), ContextValue::Number(2.into()),)])
                .unwrap()
        );

        let replaced_authorization_parameters = r#"{"resources":{"endpoints":[{"authorization":{"scheme":"bearer","parameters":{"first":"one"}},"AUTHORIZATION":{"parameters":{"second":"two"}}}]}}"#;
        let message = decode(replaced_authorization_parameters)
            .unwrap()
            .message
            .expect("repeated authorization callback replaces copied parameters");
        let resources = message.resources.unwrap();
        let authorization = resources.endpoints[0]
            .as_ref()
            .unwrap()
            .authorization
            .as_ref()
            .unwrap();
        assert_eq!(authorization.scheme.as_deref(), Some("bearer"));
        assert_eq!(
            authorization.parameters,
            BTreeMap::from([("second".to_owned(), Some("two".to_owned()))])
        );

        for later_parameters in ["null", "{}"] {
            let body = format!(
                r#"{{"resources":{{"endpoints":[{{"authorization":{{"parameters":{{"first":"one"}}}},"AUTHORIZATION":{{"parameters":{later_parameters}}}}}]}}}}"#
            );
            let message = decode(&body)
                .unwrap()
                .message
                .expect("null/empty callback map leaves copied parameters intact");
            let resources = message.resources.unwrap();
            let authorization = resources.endpoints[0]
                .as_ref()
                .unwrap()
                .authorization
                .as_ref()
                .unwrap();
            assert_eq!(
                authorization.parameters,
                BTreeMap::from([("first".to_owned(), Some("one".to_owned()))]),
                "later Parameters={later_parameters}"
            );
        }
    }

    #[test]
    fn acquired_text_reader_strings_preserve_number_lexemes_and_jobject_strings_format_values() {
        for (token, expected) in [
            ("0x10", "0x10"),
            ("0XFF", "0XFF"),
            ("010", "010"),
            ("01", "01"),
            ("1.", "1."),
            (".5", ".5"),
            ("1e309", "1e309"),
            ("-1e309", "-1e309"),
            ("1e-5000", "1e-5000"),
            ("-1e-5000", "-1e-5000"),
        ] {
            let body = format!(r#"{{"jobDisplayName":{token}}}"#);
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                &body,
            )
            .expect("CLR ReadAsString accepts primitive numbers");
            let message = decoded.message.expect("partial DTO deserializes");
            assert_eq!(
                message.job_display_name.as_deref(),
                Some(expected),
                "{token}"
            );
            assert_eq!(decoded.raw_json, body);
        }

        let body = r#"{"contextData":{"x":{"t":0,"s":0x10}}}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("JObject-backed ContextData string coercion succeeds");
        let message = decoded.message.expect("partial DTO deserializes");
        assert_eq!(message.context_data.as_ref().unwrap()["x"]["s"], "16");

        for (token, expected) in [
            ("0x10", "16"),
            ("0XFF", "255"),
            ("010", "8"),
            ("01", "1"),
            ("1.", "1"),
            (".5", "0.5"),
            ("1e309", "Infinity"),
            ("-1e309", "-Infinity"),
            ("1e-5000", "0"),
            ("-1e-5000", "-0"),
            ("1e-7", "1E-07"),
            ("1e17", "1E+17"),
        ] {
            let body = format!(r#"{{"contextData":{{"x":{{"t":0,"s":{token}}}}}}}"#);
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                &body,
            )
            .expect("JObjectReader string coercion formats the parsed token");
            let message = decoded.message.expect("ContextData string materializes");
            assert_eq!(
                message.materialize_context_data().unwrap()["x"],
                expected,
                "JObjectReader token {token}"
            );
        }
    }

    #[test]
    fn acquired_double_string_special_whitespace_and_reader_dispositions_match_clr() {
        for (value, expected) in [
            ("\u{00a0}+Infinity\u{00a0}", f64::INFINITY),
            ("\u{202f}-infinity\u{202f}", f64::NEG_INFINITY),
        ] {
            let body = format!(r#"{{"jobContainer":{{"type":6,"num":{}}}}}"#, json!(value));
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                &body,
            )
            .expect("ReadAsDouble accepts whitespace around special values");
            let message = decoded.message.expect("typed special Double materializes");
            let runtime = message.materialize_runtime().unwrap();
            assert_eq!(
                runtime.job_container.as_ref().unwrap()["num"].as_f64(),
                Some(expected)
            );
        }

        for body in [
            r#"{"jobContainer":{"type":6,"num":true}}"#,
            r#"{"jobContainer":{"type":6,"num":[]}}"#,
            r#"{"contextData":{"x":{"t":4,"n":true}}}"#,
        ] {
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .expect("JsonReaderException follows null-success disposition");
            assert!(decoded.message.is_none(), "{body}");
        }

        for body in [
            r#"{"jobContainer":{"type":6,"num":null}}"#,
            r#"{"jobContainer":{"type":6,"num":" 1.5 "}}"#,
            r#"{"contextData":{"x":{"t":4,"n":null}}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body,
                )
                .is_err(),
                "JsonSerializationException remains retryable: {body}"
            );
        }
    }

    #[test]
    fn acquire_reader_extensions_preserve_undefined_constructors_and_unknown_skips() {
        let body = "/* head */ {jobName:'run', contextData:{missing:undefined}, environmentVariables:[,], ignored:new Foo(0)} // tail";
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("comments, unquoted keys, holes, and unknown constructors are accepted");
        assert_eq!(decoded.raw_json, body);
        let message = decoded.message.expect("known DTO fields deserialize");
        assert_eq!(message.job_name.as_deref(), Some("run"));
        assert_eq!(
            message.context_data.as_ref().unwrap()["missing"],
            Value::Null
        );
        assert_eq!(message.environment_variables, vec![Value::Null]);

        let undefined_string = r#"{"jobName":undefined}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(undefined_string.len() as u64),
            Some("application/json"),
            undefined_string,
        )
        .expect("undefined String reader failures follow null-success");
        assert!(decoded.message.is_none());

        for body in [r#"{"jobName":new Foo(0)}"#, r#"{"plan":new Foo(0)}"#] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body,
                )
                .is_err(),
                "known constructor token fails DTO deserialization: {body}"
            );
        }

        let comment_between_key_and_colon = r#"{"jobName"/* gap */:"run"}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(comment_between_key_and_colon.len() as u64),
            Some("application/json"),
            comment_between_key_and_colon,
        )
        .expect("JsonTextReader syntax failures follow null-success");
        assert!(decoded.message.is_none());
    }

    #[test]
    fn acquire_wire_int64_conversion_matches_clr_before_materialization() {
        // Json.NET's Int64 conversion uses EnsureType/Convert.ChangeType and
        // rounds floating-point input (1.5 becomes 2). The ordered boundary
        // must pass that normalized value to the wire DTO, without retrying.
        let body = r#"{"jobId":"00000000-0000-0000-0000-000000000001","plan":{"planId":"00000000-0000-0000-0000-000000000002"},"requestId":1.5}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("CLR Int64 coercion succeeds");
        let message = decoded.message.expect("normalized wire DTO materializes");
        assert_eq!(message.request_id, 2);
        assert_eq!(
            decoded.identity.job_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(
            decoded.identity.plan_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000002")
        );
    }

    #[test]
    fn acquire_wire_nonfinite_literals_reach_typed_token_and_context_values() {
        let body = r#"{
            "jobId":"00000000-0000-0000-0000-000000000001",
            "jobDisplayName":NaN,
            "jobContainer":{"type":6,"num":NaN},
            "jobOutputs":Infinity,
            "contextData":{
                "nan":{"t":4,"n":NaN},
                "positive":{"t":4,"n":Infinity},
                "negative":-Infinity,
                "literal_marker":{"t":0,"s":"NaN"},
                "plain_marker":"NaN"
            }
        }"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("Newtonsoft accepts bare NaN and Infinity float tokens");
        assert_eq!(decoded.raw_json, body);
        let message = decoded
            .message
            .expect("typed nonfinite fields are materialized before acquisition returns");
        assert_eq!(message.job_display_name.as_deref(), Some("NaN"));
        let container = message.job_container.as_ref().unwrap();
        assert_eq!(container["type"], 6);
        assert_eq!(container["num"], "NaN");
        let outputs = message.job_outputs.as_ref().unwrap();
        assert_eq!(outputs["type"], 6);
        assert_eq!(outputs["num"], "Infinity");

        let context = message
            .materialize_context_values()
            .expect("typed context data materializes nonfinite doubles");
        assert!(matches!(
            context["nan"],
            velnor_model::ContextValue::NonFinite(velnor_model::NonFinite::NaN)
        ));
        assert!(matches!(
            context["positive"],
            velnor_model::ContextValue::NonFinite(velnor_model::NonFinite::PositiveInfinity)
        ));
        assert!(matches!(
            context["negative"],
            velnor_model::ContextValue::NonFinite(velnor_model::NonFinite::NegativeInfinity)
        ));
        assert!(matches!(
            &context["literal_marker"],
            velnor_model::ContextValue::String(value) if value == "NaN"
        ));
        assert!(matches!(
            &context["plain_marker"],
            velnor_model::ContextValue::String(value) if value == "NaN"
        ));

        let unquoted_nonfinite_name = r#"{NaN : 1}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(unquoted_nonfinite_name.len() as u64),
            Some("application/json"),
            unquoted_nonfinite_name,
        )
        .expect("JsonTextReader also accepts NaN as an unquoted property name");
        assert!(decoded.message.is_some());

        let unquoted_infinity_name = r#"{Infinity : 1}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(unquoted_infinity_name.len() as u64),
            Some("application/json"),
            unquoted_infinity_name,
        )
        .expect("JsonTextReader accepts Infinity as an unquoted property name");
        assert_eq!(decoded.raw["Infinity"], 1);
        assert!(decoded.message.is_some());

        let unquoted_negative_infinity_name = r#"{-Infinity:1}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(unquoted_negative_infinity_name.len() as u64),
            Some("application/json"),
            unquoted_negative_infinity_name,
        )
        .expect("negative Infinity is a JsonReaderException in property position");
        assert!(decoded.message.is_none());
    }

    #[test]
    fn acquire_wire_jtoken_roots_wrap_complete_context_trees() {
        let body = r#"{
            "resources": {
                "repositories": [{
                    "properties": {
                        "positive": Infinity,
                        "negative": -Infinity,
                        "finite": 1.25,
                        "literal": "NaN",
                        "plain_marker": "__VELNOR_CLR_NAN_0__",
                        "escaped_marker": "__VELNOR_CLR_\u004eAN_0__",
                        "bare_nan": NaN,
                        "big_integer_signed_overflow": 9223372036854775808,
                        "big_integer_unsigned_max": 18446744073709551615,
                        "big_integer": 18446744073709551616,
                        "marker": {"$velnor_context_value":"non_finite","value":"NaN"},
                        "undefined": undefined,
                        "constructor": new Foo(0, 'x',),
                        "nested_constructor": new Foo(new Bar(undefined, new Baz(3))),
                        "constructor_array": [undefined, new Ctor(1, 2)],
                        "hole_array": [,],
                        "nested": {"Case": NaN, "case": Infinity},
                        "array": [NaN, {"inside": -Infinity}],
                        Infinity : "infinity key",
                        NaN : NaN
                    }
                }],
                "endpoints": [{
                    "operationStatus": {
                        "Case": NaN,
                        "case": Infinity,
                        "finite": 2.5,
                        "big_integer": -123456789012345678901234567890,
                        "marker": {"$velnor_context_value":"non_finite","value":"NaN"},
                        "undefined": undefined,
                        "constructor": new Foo(0, new Bar(undefined))
                    }
                }]
            }
        }"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("JObject properties retain Newtonsoft nonfinite values");
        assert_eq!(decoded.raw_json, body);
        let message = decoded
            .message
            .expect("typed ContextValue trees materialize");
        let resources = message.resources.as_ref().unwrap();
        let properties = &resources.repositories[0].as_ref().unwrap().properties;

        assert_eq!(properties.is_case_sensitive(), Some(false));
        assert!(matches!(
            properties.get("NAN"),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            properties.get("positive"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(matches!(
            properties.get("negative"),
            Some(ContextValue::NonFinite(NonFinite::NegativeInfinity))
        ));
        assert!(matches!(
            properties.get("finite"),
            Some(ContextValue::Number(value)) if value.to_string() == "1.25"
        ));
        assert!(matches!(
            properties.get("literal"),
            Some(ContextValue::String(value)) if value == "NaN"
        ));
        assert!(matches!(
            properties.get("plain_marker"),
            Some(ContextValue::String(value)) if value == "__VELNOR_CLR_NAN_0__"
        ));
        assert!(matches!(
            properties.get("escaped_marker"),
            Some(ContextValue::String(value)) if value == "__VELNOR_CLR_NAN_0__"
        ));
        assert!(matches!(
            properties.get("bare_nan"),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            properties.get("big_integer_signed_overflow"),
            Some(ContextValue::BigInteger(value)) if value == "9223372036854775808"
        ));
        assert!(matches!(
            properties.get("big_integer_unsigned_max"),
            Some(ContextValue::BigInteger(value)) if value == "18446744073709551615"
        ));
        assert!(matches!(
            properties.get("big_integer"),
            Some(ContextValue::BigInteger(value)) if value == "18446744073709551616"
        ));
        assert!(matches!(
            properties.get("undefined"),
            Some(ContextValue::Undefined)
        ));
        assert!(matches!(
            properties.get("constructor"),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo"
                    && matches!(arguments.as_slice(), [
                        ContextValue::Number(first),
                        ContextValue::String(second),
                    ] if first.as_i64() == Some(0) && second == "x")
        ));
        assert!(matches!(
            properties.get("constructor_array"),
            Some(ContextValue::Array(values))
                if matches!(values.as_slice(), [
                    ContextValue::Undefined,
                    ContextValue::Constructor { name, arguments },
                ] if name == "Ctor" && arguments.len() == 2)
        ));
        assert!(matches!(
            properties.get("nested_constructor"),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo" && matches!(arguments.as_slice(), [
                    ContextValue::Constructor { name, arguments }
                ] if name == "Bar" && matches!(arguments.as_slice(), [
                    ContextValue::Undefined,
                    ContextValue::Constructor { name, arguments }
                ] if name == "Baz" && matches!(arguments.as_slice(), [ContextValue::Number(value)] if value.as_i64() == Some(3))))
        ));
        assert!(matches!(
            properties.get("hole_array"),
            Some(ContextValue::Array(values))
                if matches!(values.as_slice(), [ContextValue::Undefined])
        ));
        assert!(matches!(
            properties.get("infinity"),
            Some(ContextValue::String(value)) if value == "infinity key"
        ));
        let marker = properties.get("marker").expect("marker-shaped user object");
        assert!(matches!(marker, ContextValue::Object { .. }));
        assert_eq!(
            marker.get("$velnor_context_value"),
            Some(&ContextValue::String("non_finite".to_owned()))
        );
        assert_eq!(
            marker.get("value"),
            Some(&ContextValue::String("NaN".to_owned()))
        );

        let nested = properties.get("nested").unwrap();
        assert_eq!(nested.is_case_sensitive(), Some(true));
        assert!(matches!(
            nested.get("Case"),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            nested.get("case"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(nested.get("CASE").is_none());

        let array = properties.get("array").unwrap();
        assert!(matches!(
            array,
            ContextValue::Array(values)
                if matches!(values.first(), Some(ContextValue::NonFinite(NonFinite::NaN)))
                    && matches!(values.get(1).and_then(|value| value.get("inside")), Some(ContextValue::NonFinite(NonFinite::NegativeInfinity)))
        ));
        let endpoint = resources.endpoints[0].as_ref().unwrap();
        let operation_status = endpoint
            .operation_status
            .as_ref()
            .expect("OperationStatus is a JObject context");
        assert_eq!(operation_status.is_case_sensitive(), Some(true));
        assert!(matches!(
            operation_status.get("Case"),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            operation_status.get("case"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(operation_status.get("CASE").is_none());
        assert!(matches!(
            operation_status.get("finite"),
            Some(ContextValue::Number(value)) if value.to_string() == "2.5"
        ));
        assert!(matches!(
            operation_status.get("big_integer"),
            Some(ContextValue::BigInteger(value)) if value == "-123456789012345678901234567890"
        ));
        assert!(matches!(
            operation_status.get("marker"),
            Some(ContextValue::Object { .. })
        ));
        assert!(matches!(
            operation_status.get("undefined"),
            Some(ContextValue::Undefined)
        ));
        assert!(matches!(
            operation_status.get("constructor"),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo" && matches!(arguments.as_slice(), [
                    ContextValue::Number(value),
                    ContextValue::Constructor { name, arguments }
                ] if value.as_i64() == Some(0) && name == "Bar" && matches!(arguments.as_slice(), [ContextValue::Undefined]))
        ));

        for body in [
            r#"{"resources":{"repositories":[{"properties":[]}]}}"#,
            r#"{"resources":{"endpoints":[{"operationStatus":[]}]}}"#,
            r#"{"resources":{"endpoints":[{"operationStatus":1}]}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "typed JObject/dictionary serialization failures remain retryable: {body}"
            );
        }
    }

    #[test]
    fn acquire_wire_big_integer_bounds_match_newtonsoft_reader() {
        let body_for_integer = |integer: &str| {
            format!(
                "{}{}{}",
                r#"{"resources":{"repositories":[{"properties":{"huge_integer":"#,
                integer,
                r#"}}]}}"#
            )
        };

        let accepted = ["9".repeat(380), format!("-{}", "9".repeat(379))];
        for integer in accepted {
            let body = body_for_integer(&integer);
            let payload = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                &body,
            )
            .expect("JsonTextReader accepts integer lexemes through 380 chars");
            assert_eq!(payload.raw_json, body);
            let message = payload.message.expect("BigInteger JToken materializes");
            let properties = &message.resources.as_ref().unwrap().repositories[0]
                .as_ref()
                .unwrap()
                .properties;
            assert!(matches!(
                properties.get("huge_integer"),
                Some(ContextValue::BigInteger(value)) if value == &integer
            ));
        }

        let rejected = body_for_integer(&format!("-{}", "9".repeat(380)));
        let payload = decode_acquire_job_success_body(
            200,
            Some(rejected.len() as u64),
            Some("application/json"),
            &rejected,
        )
        .expect("JsonTextReader reader errors become null-success");
        assert_eq!(payload.raw, Value::Null);
        assert!(payload.message.is_none());
    }

    #[test]
    fn acquire_wire_nonfinite_reader_and_uri_dispositions_match_clr() {
        for body in [
            r#"{"requestId":NaN}"#,
            r#"{"lockedUntil":Infinity}"#,
            r#"{"plan":{"version":-Infinity}}"#,
            r#"{"enableDebugger":NaN}"#,
        ] {
            let decoded = decode_acquire_job_success_body(
                200,
                Some(body.len() as u64),
                Some("application/json"),
                body,
            )
            .expect("JsonReaderException maps to null-success");
            assert!(decoded.message.is_none(), "body={body}");
            assert!(decoded.identity.job_id.is_none(), "body={body}");
        }
        for body in [
            r#"{"jobId":NaN}"#,
            r#"{"resources":{"endpoints":[{"url":Infinity}]}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body
                )
                .is_err(),
                "typed CLR conversion failures remain retryable: {body}"
            );
        }
    }

    #[test]
    fn acquire_wire_dto_preserves_omitted_defaults_and_nullable_references() {
        let wire: ClrAgentJobRequestMessage = serde_json::from_value(json!({})).unwrap();

        assert_eq!(wire.job_id.0, Uuid::nil());
        assert_eq!(wire.request_id.0, 0);
        assert!(!wire.enable_debugger);
        assert!(wire.message_type.is_none());
        assert!(wire.plan.is_none());
        assert!(wire.timeline.is_none());

        let nullable_references: ClrAgentJobRequestMessage = serde_json::from_value(json!({
            "messageType": null,
            "plan": null,
            "timeline": null,
            "jobDisplayName": null,
            "jobName": null,
            "jobContainer": null,
            "jobServiceContainers": null,
            "jobOutputs": null,
            "resources": null,
            "contextData": null,
            "workspace": null,
            "actionsEnvironment": null,
            "snapshot": null,
            "billingOwnerId": null,
            "debuggerTunnel": null,
            "debuggerWelcomeMessage": null,
            "jobSidecarContainers": null
        }))
        .unwrap();
        assert!(nullable_references.message_type.is_none());
        assert!(nullable_references.plan.is_none());
        assert!(nullable_references.timeline.is_none());
        assert!(nullable_references.job_display_name.is_none());
        assert!(nullable_references.job_name.is_none());
        assert!(nullable_references.job_container.is_none());
        assert!(nullable_references.job_service_containers.is_none());
        assert!(nullable_references.job_outputs.is_none());
        assert!(nullable_references.resources.is_none());
        assert!(nullable_references.context_data.is_none());
        assert!(nullable_references.workspace.is_none());
        assert!(nullable_references.actions_environment.is_none());
        assert!(nullable_references.snapshot.is_none());
        assert!(nullable_references.billing_owner_id.is_none());
        assert!(nullable_references.debugger_tunnel.is_none());
        assert!(nullable_references.debugger_welcome_message.is_none());
        assert!(nullable_references.job_sidecar_containers.is_none());

        let null_collections: ClrAgentJobRequestMessage = serde_json::from_value(json!({
            "environmentVariables": null,
            "variables": null,
            "mask": null,
            "steps": null,
            "defaults": null,
            "dependencies": null,
            "fileTable": null,
            "resources": {
                "endpoints": null,
                "repositories": null,
                "containers": null
            },
            "debuggerTunnel": {
                "tunnelId": null,
                "clusterId": null,
                "hostToken": null
            },
            "plan": {
                "scopeIdentifier": "00000000-0000-0000-0000-000000000001",
                "planId": "00000000-0000-0000-0000-000000000002",
                "owner": {"id": 0, "name": null, "_links": null}
            },
            "timeline": {"id": "00000000-0000-0000-0000-000000000003"},
            "workspace": {"clean": null}
        }))
        .unwrap();
        assert!(null_collections.environment_variables.is_empty());
        assert!(null_collections.variables.is_empty());
        assert!(null_collections.mask_hints.is_empty());
        assert!(null_collections.defaults.is_empty());
        assert!(null_collections.actions_dependencies.is_empty());
        assert!(null_collections.file_table.is_empty());
        let resources = null_collections.resources.unwrap();
        assert!(resources.endpoints.is_empty());
        assert!(resources.repositories.is_empty());
        assert!(resources.containers.is_empty());
        assert_eq!(null_collections.debugger_tunnel.unwrap().port, 0);
        let plan = null_collections.plan.unwrap();
        assert_eq!(
            plan.scope_identifier.0,
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
        );
        assert_eq!(
            plan.plan_id.0,
            Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
        );
        assert_eq!(plan.version.0, 0);
        assert_eq!(plan.owner.unwrap().id.0, 0);
        let timeline = null_collections.timeline.unwrap();
        assert_eq!(
            timeline.id.0,
            Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()
        );
        assert_eq!(timeline.change_id.0, 0);
        assert!(null_collections.workspace.unwrap().clean.is_none());
        assert!(null_collections.steps.is_empty());

        let missing_step_members: ClrAgentJobRequestMessage =
            serde_json::from_value(json!({"steps": [{"type": "Action"}]})).unwrap();
        let step = missing_step_members.steps[0].as_ref().unwrap();
        assert_eq!(step.id.0, Uuid::nil());
        assert!(step.enabled);

        let nullable_collection_values: ClrAgentJobRequestMessage = serde_json::from_value(json!({
            "variables": {"null-value": null, "value": {"value": null}},
            "mask": [null, {}, {"type": "Variable", "value": null}],
            "steps": [
                null,
                {},
                {
                    "type": "Action",
                    "name": null,
                    "displayName": null,
                    "condition": null,
                    "continueOnError": null,
                    "timeoutInMinutes": null,
                    "parallelGroupId": null,
                    "reference": null,
                    "displayNameToken": null,
                    "contextName": null,
                    "environment": null,
                    "inputs": null
                },
                {
                    "type": "BackgroundStepControl",
                    "controlType": null,
                    "stepIds": [null],
                    "displayNameToken": null
                }
            ],
            "environmentVariables": [null],
            "defaults": [null],
            "dependencies": [null],
            "fileTable": [null],
            "jobSidecarContainers": {"sidecar": null},
            "resources": {
                "endpoints": [null, {
                    "name": null,
                    "type": null,
                    "owner": null,
                    "url": null,
                    "description": null,
                    "authorization": {"scheme": null, "parameters": null},
                    "data": null,
                    "operationStatus": null,
                    "isReady": null
                }, {"authorization": null}],
                "repositories": [null, {
                    "alias": null,
                    "endpoint": null,
                    "properties": {}
                }, {
                    "endpoint": {"id": "00000000-0000-0000-0000-000000000004"},
                    "properties": {}
                }],
                "containers": [null, {
                    "alias": null,
                    "endpoint": null,
                    "properties": {}
                }]
            },
            "actionsEnvironment": {"name": null, "url": null},
            "plan": {
                "planType": null,
                "planGroup": null,
                "artifactUri": null,
                "artifactLocation": null,
                "definition": null,
                "owner": null
            },
            "timeline": {"location": null}
        }))
        .unwrap();
        assert!(nullable_collection_values.variables["null-value"].is_none());
        assert!(nullable_collection_values.variables["value"]
            .as_ref()
            .unwrap()
            .value
            .is_none());
        assert_eq!(nullable_collection_values.mask_hints.len(), 3);
        assert!(nullable_collection_values.mask_hints[0].is_none());
        assert_eq!(nullable_collection_values.steps.len(), 4);
        assert!(nullable_collection_values.steps[0].is_none());
        assert!(nullable_collection_values.steps[1].is_none());
        assert!(nullable_collection_values.steps[2].is_some());
        assert!(nullable_collection_values.steps[3].is_some());
        assert_eq!(nullable_collection_values.environment_variables.len(), 1);
        assert!(nullable_collection_values.environment_variables[0].is_none());
        assert!(nullable_collection_values.defaults[0].is_none());
        assert!(nullable_collection_values.actions_dependencies[0].is_none());
        assert!(nullable_collection_values.file_table[0].is_none());
        assert!(nullable_collection_values
            .job_sidecar_containers
            .as_ref()
            .unwrap()["sidecar"]
            .is_none());
        let resources = nullable_collection_values.resources.unwrap();
        assert!(resources.endpoints[0].is_none());
        assert!(resources.endpoints[1]
            .as_ref()
            .unwrap()
            .authorization
            .as_ref()
            .unwrap()
            .parameters
            .is_empty());
        assert!(resources.endpoints[1].as_ref().unwrap().is_ready);
        assert!(resources.endpoints[1].as_ref().unwrap().data.is_none());
        assert!(resources.endpoints[1].as_ref().unwrap().name.is_none());
        assert!(resources.endpoints[2].as_ref().unwrap().name.is_none());
        assert!(resources.endpoints[2]
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .is_empty());
        assert!(resources.endpoints[2]
            .as_ref()
            .unwrap()
            .authorization
            .is_none());
        assert!(resources.repositories[0].is_none());
        assert!(resources.containers[0].is_none());
        assert_eq!(
            resources.repositories[2]
                .as_ref()
                .unwrap()
                .endpoint
                .as_ref()
                .unwrap()
                .id
                .0,
            Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap()
        );
        assert_eq!(
            resources.repositories[2]
                .as_ref()
                .unwrap()
                .endpoint
                .as_ref()
                .unwrap()
                .name,
            ClrExpressionValueString::NullReference
        );
        assert!(nullable_collection_values
            .plan
            .unwrap()
            .definition
            .is_none());
        assert!(nullable_collection_values
            .timeline
            .unwrap()
            .location
            .is_none());

        for value in [
            json!({"jobId": null}),
            json!({"requestId": null}),
            json!({"lockedUntil": null}),
            json!({"enableDebugger": null}),
            json!({"debuggerTunnel": {"port": null}}),
            json!({"plan": {"scopeIdentifier": null}}),
            json!({"plan": {"planId": null}}),
            json!({"plan": {"version": null}}),
            json!({"plan": {"owner": {"id": null}}}),
            json!({"plan": {"definition": {"id": null}}}),
            json!({"timeline": {"id": null}}),
            json!({"timeline": {"changeId": null}}),
            json!({"variables": {"secret": {"isSecret": null}}}),
            json!({"mask": [{"type": null}]}),
            json!({"steps": [{"type": "Action", "id": null}]}),
            json!({"steps": [{"type": "Action", "enabled": null}]}),
            json!({"steps": [{"type": "Action", "background": null}]}),
            json!({"resources": {"endpoints": [{"id": null}]}}),
            json!({"resources": {"endpoints": [{"groupScopeId": null}]}}),
            json!({"resources": {"endpoints": [{"isShared": null}]}}),
            json!({"resources": {"repositories": [{"endpoint": {"id": null}, "properties": {}}]}}),
        ] {
            let body = value.to_string();
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    &body
                )
                .is_err(),
                "explicit null must not be replaced with a non-nullable CLR value default: {value}"
            );
        }
    }

    #[test]
    fn acquire_wire_resources_accept_arbitrary_property_bags_and_reject_typed_nested_mismatches() {
        let body = json!({
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "properties": {
                        "id": 42,
                        "cloneUrl": "https://github.com/owner/repo.git",
                        "opaque": {"enabled": true, "items": [null, 1, "x"]}
                    }
                }],
                "containers": [{
                    "alias": "job",
                    "properties": {"image": "alpine", "arbitrary": [1, {"x": false}]}
                }]
            }
        });
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.to_string().len() as u64),
            Some("application/json"),
            &body.to_string(),
        )
        .expect("ResourceProperties contains arbitrary JToken values");
        assert_eq!(
            decoded.raw["resources"]["repositories"][0]["properties"]["id"],
            42
        );
        assert_eq!(
            decoded.raw["resources"]["repositories"][0]["properties"]["opaque"]["items"][0],
            Value::Null
        );

        for invalid_nested in [
            json!({"resources": {"endpoints": [{"id": []}]}}),
            json!({"steps": [{"type": 4, "enabled": "not-bool"}]}),
            json!({"steps": [{"type": 4, "reference": {"type": 1, "path": []}}]}),
        ] {
            assert!(
                serde_json::from_value::<ClrAgentJobRequestMessage>(invalid_nested.clone())
                    .is_err(),
                "nested CLR typed members reject wrong shapes: {invalid_nested}"
            );
        }

        assert!(serde_json::from_value::<ClrAgentJobRequestMessage>(json!({
            "resources": {"repositories": [{"properties": null}]}
        }))
        .is_err());

        for body in [
            r#"{"resources":{"repositories":[{"properties":undefined}]}}"#,
            r#"{"resources":{"repositories":[{"properties":new Foo()}]}}"#,
            r#"{"resources":{"endpoints":[{"operationStatus":undefined}]}}"#,
            r#"{"resources":{"endpoints":[{"operationStatus":new Foo()}]}}"#,
        ] {
            assert!(
                decode_acquire_job_success_body(
                    200,
                    Some(body.len() as u64),
                    Some("application/json"),
                    body,
                )
                .is_err(),
                "typed JToken root rejects non-object value: {body}"
            );
        }

        let null_status = r#"{"resources":{"endpoints":[{"operationStatus":null}]}}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(null_status.len() as u64),
            Some("application/json"),
            null_status,
        )
        .unwrap();
        let message = decoded.message.unwrap().materialize_runtime().unwrap();
        assert!(message.resources.endpoints[0].operation_status.is_none());
    }

    #[test]
    fn context_root_case_collisions_keep_first_slot_and_last_source_value() {
        let body = r#"{"contextData":{"root":{"t":0,"s":"first"},"Root":{"t":0,"s":"later"}}}"#;
        let decoded = decode_acquire_job_success_body(
            200,
            Some(body.len() as u64),
            Some("application/json"),
            body,
        )
        .expect("the acquired CLR DTO accepts distinct exact-case roots");
        let job = decoded
            .message
            .expect("the Velnor wire DTO retains ordered ContextData roots")
            .materialize_runtime()
            .unwrap();
        let context = crate::runner::job_context_data(&job).unwrap();
        let root_entries: Vec<_> = context
            .iter()
            .filter(|(name, _)| crate::job_message::clr_ordinal_ignore_case_eq(name, "root"))
            .collect();
        assert_eq!(root_entries.len(), 1);
        assert_eq!(root_entries[0].0, "root");
        assert_eq!(
            root_entries[0].1,
            velnor_model::ContextValue::String("later".to_owned())
        );
    }

    #[test]
    fn acquire_retry_delay_is_uniform_integer_milliseconds_in_upstream_range() {
        let client = RunServiceClient::new("token").unwrap();
        for _ in 0..1_000 {
            let delay = client.acquire_retry_delay(1);
            assert!(delay.as_millis() >= 5_000);
            assert!(delay.as_millis() < 15_000);
        }
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_retries_untyped_4xx_and_exhausts() {
        use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad credentials"))
            .expect(5)
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
            .expect_err("generic acquire failures must retry up to the local attempt bound");

        assert!(is_transient_acquire_error(&error));
        assert!(
            error
                .to_string()
                .contains("transient run-service acquire failure after retries"),
            "{error:#}"
        );
        assert_eq!(
            broker_error_category(&error),
            Some(BrokerErrorCategory::Transient)
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn acquire_job_exhausted_transient_carries_boundary_category() {
        // Regression (r0-798-corr): before the correction the exhausted
        // transient budget surfaced an untyped error (`broker_error_category`
        // returned `None` while the session gate re-derived the verdict from
        // the wrapper). The produced error now carries the category the loop
        // decided on, and the session stays alive as before.
        use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(ResponseTemplate::new(503).set_body_string("try later"))
            .expect(5)
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
            .expect_err("an always-503 acquire must exhaust the retry budget");

        assert!(is_transient_acquire_error(&error));
        assert_eq!(
            broker_error_category(&error),
            Some(BrokerErrorCategory::Transient)
        );
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
            runner_os: "Linux",
            billing_owner_id: Some("42"),
        })
        .unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "jobMessageId": "request-1",
                "runnerOS": "Linux",
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
    fn task_result_parse_wire_accepts_both_casings() {
        for (raw, expected) in [
            ("succeeded", TaskResult::Succeeded),
            ("Succeeded", TaskResult::Succeeded),
            ("failed", TaskResult::Failed),
            ("Failed", TaskResult::Failed),
            ("canceled", TaskResult::Canceled),
            ("Canceled", TaskResult::Canceled),
            ("skipped", TaskResult::Skipped),
            ("Skipped", TaskResult::Skipped),
            ("abandoned", TaskResult::Abandoned),
            ("Abandoned", TaskResult::Abandoned),
        ] {
            assert_eq!(TaskResult::parse_wire(raw), Some(expected), "{raw}");
            let parsed: TaskResult = serde_json::from_value(serde_json::json!(raw)).unwrap();
            assert_eq!(
                parsed, expected,
                "serde must agree with parse_wire for {raw}"
            );
        }
        for raw in [
            "",
            "cancelled",
            "CANCELED",
            "success",
            " canceled",
            "canceled ",
        ] {
            assert_eq!(TaskResult::parse_wire(raw), None, "{raw}");
            assert!(
                serde_json::from_value::<TaskResult>(serde_json::json!(raw)).is_err(),
                "serde must reject what parse_wire rejects: {raw}"
            );
        }
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
            1_000,
        );
        assert_eq!(hint.retry_after_seconds, Some(17));
        assert_eq!(hint.rate_limit_reset_epoch, Some(1060));
        assert_eq!(hint.remaining, Some(0));
        assert_eq!(hint.delay(1000), Some(std::time::Duration::from_secs(60)));

        let error = github_api_error_with_retry("quota", 403, "exhausted", hint);
        assert_eq!(
            github_api_retry_delay_at(&error, 1_000),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(
            github_api_quota_status(&error).and_then(|status| status.remaining),
            Some(0)
        );

        let reset_only = github_api_error_with_retry(
            "quota",
            403,
            "reset",
            GitHubRetryHint {
                retry_after_seconds: None,
                rate_limit_reset_epoch: Some(1060),
                remaining: Some(0),
            },
        );
        assert_eq!(
            github_api_retry_delay_at(&reset_only, 1_000),
            Some(std::time::Duration::from_secs(60))
        );

        let retry_after_only = github_api_error_with_retry(
            "quota",
            403,
            "retry",
            GitHubRetryHint {
                retry_after_seconds: Some(17),
                rate_limit_reset_epoch: None,
                remaining: Some(0),
            },
        );
        assert_eq!(
            github_api_retry_delay_at(&retry_after_only, 1_000),
            Some(std::time::Duration::from_secs(17))
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
        assert_eq!(
            github_api_retry_delay_at(&permission, 1_700_000_000),
            None,
            "permission 403 reset headers must not delay the slot"
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
            github_retry_hint_from_header_map(&headers, 1_000),
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
            1_000,
        );
        assert_eq!(hint, GitHubRetryHint::default());
        assert_eq!(hint.delay(1000), None);
    }

    #[test]
    fn retry_after_is_honored_for_transient_server_errors_without_using_reset() {
        let error = github_api_error_with_retry(
            "transient",
            503,
            "unavailable",
            GitHubRetryHint {
                retry_after_seconds: Some(17),
                rate_limit_reset_epoch: Some(1_800_000_000),
                remaining: Some(4999),
            },
        );
        assert_eq!(
            github_api_retry_delay_at(&error, 1_700_000_000),
            Some(Duration::from_secs(17))
        );
    }

    #[test]
    fn authenticated_endpoint_validation_rejects_cleartext_remote_urls() {
        assert!(validate_authenticated_url("http://github.example.com/api")
            .unwrap_err()
            .to_string()
            .contains("HTTPS"));
        assert!(validate_authenticated_url("https://user:pass@example.com")
            .unwrap_err()
            .to_string()
            .contains("userinfo"));
        let loopback = validate_authenticated_url("http://127.0.0.1:8080/api");
        if cfg!(feature = "test-support") {
            assert!(loopback.is_ok());
        } else {
            assert!(loopback.unwrap_err().to_string().contains("HTTPS"));
        }
    }

    #[test]
    fn github_actions_service_host_accepts_regional_single_label_hosts() {
        for host in [
            "pipelines.actions.githubusercontent.com",
            "pipelinesghubeus14.actions.githubusercontent.com",
            "BROKER.ACTIONS.GITHUBUSERCONTENT.COM",
        ] {
            assert!(
                is_github_actions_service_host(host),
                "expected GitHub Actions host to be accepted: {host}"
            );
        }

        for host in [
            "actions.githubusercontent.com",
            "pipelinesghubeus14.actions.githubusercontent.com.evil.example",
            "nested.pipelinesghubeus14.actions.githubusercontent.com",
            "-pipelines.actions.githubusercontent.com",
            "pipelines-.actions.githubusercontent.com",
        ] {
            assert!(
                !is_github_actions_service_host(host),
                "expected non-service host to be rejected: {host}"
            );
        }
    }

    #[test]
    fn known_service_validation_accepts_regional_actions_hosts() {
        let url = validate_known_service_url(
            "https://pipelinesghubeus14.actions.githubusercontent.com/tenant",
            "ServerUrl",
            &["pipelines.actions.githubusercontent.com"],
        )
        .unwrap();
        assert_eq!(
            url.host_str(),
            Some("pipelinesghubeus14.actions.githubusercontent.com")
        );
    }

    #[test]
    fn signed_blob_validation_requires_secure_url_and_redacts_credentials() {
        let loopback = validate_signed_blob_url("http://127.0.0.1:8080/blob", "artifact");
        if cfg!(feature = "test-support") {
            assert!(loopback.is_ok());
        } else {
            assert!(loopback
                .unwrap_err()
                .chain()
                .any(|cause| cause.to_string().contains("HTTPS")));
        }
        assert!(validate_signed_blob_url("http://blob.example.com/blob", "artifact").is_err());
        assert!(
            validate_signed_blob_url("https://user:pass@blob.example.com/blob", "artifact")
                .is_err()
        );

        let safe = redacted_authenticated_url(
            "https://user:pass@blob.example.com/blob?sig=secret&token=secret#fragment",
        );
        assert_eq!(safe, "https://blob.example.com/blob");
        assert!(!safe.contains("secret"));
    }

    #[test]
    fn provider_error_bodies_are_bounded_and_secret_safe() {
        let error = github_api_error(
            "test request",
            500,
            "authorization: Bearer reflected-secret",
        );
        let error = error.downcast_ref::<GitHubApiError>().unwrap();
        assert_eq!(error.body, "<redacted response body>");

        let long = "x".repeat(5000);
        let error = github_api_error("test request", 500, long);
        let error = error.downcast_ref::<GitHubApiError>().unwrap();
        assert!(error.body.len() <= 4099);
        assert!(error.body.ends_with('…'));
    }

    #[test]
    fn protocol_debug_redacts_secrets_and_sanitizes_signed_urls() {
        let scope = GitHubScope::parse(
            "https://scope-user:scope-pass@github.com/org?token=scope-query-secret#scope-fragment-secret",
        )
        .unwrap();
        let api_error = GitHubApiError {
            status: 500,
            action: "request".to_string(),
            body: "api-body-secret".to_string(),
            retry_after_seconds: None,
            rate_limit_reset_epoch: None,
            remaining: None,
            category: None,
        };
        let jit_response = GitHubJitConfigResponse {
            runner: GitHubJitRunner {
                id: 1,
                name: "runner".to_string(),
                os: "linux".to_string(),
                status: "offline".to_string(),
                busy: false,
                labels: Vec::new(),
                runner_group_id: None,
                ephemeral: None,
            },
            encoded_jit_config: "encoded-jit-secret".to_string(),
        };
        let settings = DecodedJitRunnerSettings {
            agent_id: None,
            agent_name: None,
            pool_id: None,
            pool_name: None,
            server_url: Some("https://server.example/agent?token=settings-url-secret".to_string()),
            server_url_v2: None,
            github_url: None,
            work_folder: None,
            use_v2_flow: false,
            ephemeral: false,
            disable_update: false,
        };
        let credentials: DecodedJitCredentials = serde_json::from_value(json!({
            "Scheme": "OAuth",
            "Data": { "token": "decoded-credential-secret" }
        }))
        .unwrap();
        let decoded = DecodedJitConfig {
            settings,
            credentials,
            private_key_pem: "decoded-private-key-secret".to_string(),
        };
        let oauth_jwt = OAuthJwtCredentials {
            client_id: "client-id".to_string(),
            authorization_url: "https://oauth.example/token?sig=oauth-url-secret".to_string(),
            private_key_pem: "oauth-private-key-secret".to_string(),
        };
        let oauth_response = OAuthTokenResponse {
            access_token: Some("oauth-response-token-secret".to_string()),
            token_type: Some("bearer".to_string()),
            expires_in: Some(300),
            error: None,
            error_description: None,
        };
        let oauth_access = OAuthAccessToken {
            token: "oauth-access-token-secret".to_string(),
            expires_in: Some(Duration::from_secs(300)),
        };
        let runner_key_pair = RunnerKeyPair {
            private_key_pem: "runner-private-key-secret".to_string(),
            public_key: TaskAgentPublicKey {
                exponent: "AQAB".to_string(),
                modulus: "public-modulus".to_string(),
            },
        };
        let mut task_session = TaskAgentSession::new("owner", 1, "agent");
        task_session.encryption_key = Some(TaskAgentSessionKey {
            encrypted: true,
            value: "task-session-key-secret".to_string(),
        });
        let agent_session = AgentSession {
            session_id: "session-id".to_string(),
            encryption_key: Some(EncryptionKey {
                encrypted: true,
                value_base64: "agent-session-key-secret".to_string(),
            }),
        };
        let message = TaskAgentMessage {
            message_id: 1,
            message_type: RUNNER_JOB_REQUEST.to_string(),
            body: "task-message-body-secret".to_string(),
            iv_base64: Some("message-iv".to_string()),
        };
        let secret_variable = RunServiceVariableValue {
            value: "secret-variable-value".to_string(),
            is_secret: true,
        };
        let public_variable = RunServiceVariableValue {
            value: "public-variable-value".to_string(),
            is_secret: false,
        };
        let secret_output = JobOutputValue {
            value: Some("secret-output-value".to_string()),
            is_secret: true,
        };
        let public_output = JobOutputValue {
            value: Some("public-output-value".to_string()),
            is_secret: false,
        };

        let debug = format!(
            "{scope:?} {api_error:?} {jit_response:?} {decoded:?} {oauth_jwt:?} \
             {oauth_response:?} {oauth_access:?} {runner_key_pair:?} {task_session:?} \
             {agent_session:?} {message:?} {secret_variable:?} {public_variable:?} \
             {secret_output:?} {public_output:?}"
        );

        for secret in [
            "scope-user",
            "scope-pass",
            "scope-query-secret",
            "scope-fragment-secret",
            "api-body-secret",
            "encoded-jit-secret",
            "settings-url-secret",
            "decoded-credential-secret",
            "decoded-private-key-secret",
            "oauth-url-secret",
            "oauth-private-key-secret",
            "oauth-response-token-secret",
            "oauth-access-token-secret",
            "runner-private-key-secret",
            "task-session-key-secret",
            "agent-session-key-secret",
            "task-message-body-secret",
            "secret-variable-value",
            "secret-output-value",
        ] {
            assert!(!debug.contains(secret), "Debug leaked {secret}: {debug}");
        }
        assert!(debug.contains("public-variable-value"));
        assert!(debug.contains("public-output-value"));
        assert!(debug.contains("public-modulus"));
    }
}
