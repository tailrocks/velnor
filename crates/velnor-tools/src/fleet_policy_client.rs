//! Typed GitHub org runner-group policy client behind an HTTP trait seam
//! (Plan 039 Step 2b).
//!
//! The [`FleetHttp`] trait is the only network surface: the live
//! implementation wraps `reqwest` and is constructed exclusively by the CLI
//! commands after reading credentials from the environment; tests inject
//! in-memory fakes and never perform real HTTP. Every error or report that
//! can carry request context passes through [`Secrets::redact`], so tokens
//! never reach output, logs, or error chains.
//!
//! Endpoint contract (GitHub REST, `X-GitHub-Api-Version: 2026-03-10`,
//! docs fetched 2026-08-24):
//! - group restriction fields (`visibility`, `allows_public_repositories`,
//!   `restricted_to_workflows`, `selected_workflows`) are written with
//!   `PATCH /orgs/{org}/actions/runner-groups/{id}`
//! - the exact selected repository set is replaced with
//!   `PUT /orgs/{org}/actions/runner-groups/{id}/repositories`
//!   (body `{selected_repository_ids: [int]}`)
//! - readback uses `GET .../runner-groups/{id}` plus paginated
//!   `GET .../runner-groups/{id}/repositories`.

use crate::fleet_policy::{
    semantic_diff, verify_plan_digest, ObservedGroup, OrgPolicy, WorkflowIdentity,
    GITHUB_API_VERSION,
};
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
    time::Duration,
};
use velnor_model::redaction::{SecretMasker, REDACTION};

const PER_PAGE: u32 = 100;
const MAX_PAGES: u32 = 1000;
const MAX_ATTEMPTS: u32 = 4;
const PAUSE_BETWEEN_ATTEMPTS: Duration = Duration::from_millis(250);
const MAX_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(30);
const ERROR_DETAIL_LIMIT: usize = 200;
pub(crate) const DEFAULT_GITHUB_API_URL: &str = "https://api.github.com";
const HTTP_TIMEOUT_SECONDS: u64 = 20;

// ---------------------------------------------------------------------------
// HTTP seam
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetHttpMethod {
    Get,
    Patch,
    Put,
}

impl FleetHttpMethod {
    #[cfg(test)]
    fn as_str(self) -> &'static str {
        match self {
            FleetHttpMethod::Get => "GET",
            FleetHttpMethod::Patch => "PATCH",
            FleetHttpMethod::Put => "PUT",
        }
    }
}

/// Fully-qualified API path plus query pairs; never carries credentials.
#[derive(Debug, Clone)]
pub struct FleetHttpRequest {
    pub method: FleetHttpMethod,
    pub url_path: String,
    pub query: Vec<(String, String)>,
    pub json_body: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct FleetHttpResponse {
    pub status: u16,
    /// Lowercased header names.
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl FleetHttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Transport-level failure only; HTTP status errors travel as responses so
/// retry classification stays on the gateway.
#[derive(Debug, Clone)]
pub struct FleetHttpError {
    pub message: String,
}

/// Async HTTP seam. Live impl wraps reqwest; tests inject fakes. No test may
/// construct a real network client.
pub trait FleetHttp {
    fn execute(
        &self,
        request: FleetHttpRequest,
    ) -> impl std::future::Future<Output = Result<FleetHttpResponse, FleetHttpError>> + Send;
}

// ---------------------------------------------------------------------------
// Secret redaction
// ---------------------------------------------------------------------------

fn bearer_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._\-]+").expect("valid regex"))
}

fn token_shape_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\b(gh[pousr]_[A-Za-z0-9_]{16,}|github_pat_[A-Za-z0-9_]{16,})\b")
            .expect("valid regex")
    })
}

/// Holds every credential value known to this client and scrubs them from any
/// text that could reach output or errors. Token-shaped strings are scrubbed
/// even when not registered.
#[derive(Debug, Clone)]
pub struct Secrets {
    values: Vec<String>,
}

impl Secrets {
    pub fn new(value: &str) -> Self {
        Self {
            values: vec![value.to_owned()],
        }
    }

    /// Scrub every registered credential and every token-shaped string.
    ///
    /// Registered values go through the one shared masker
    /// (`velnor_model::redaction`), so this client uses the same sentinel and
    /// the same encoded-variant and multi-line rules as the runner, the log
    /// service and the durable store validator. The token-shape and bearer
    /// patterns stay: they catch credentials this client was never told
    /// about.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let out = SecretMasker::new(self.values.iter()).mask(text);
        let out = token_shape_pattern()
            .replace_all(&out, REDACTION)
            .into_owned();
        bearer_pattern()
            .replace_all(&out, format!("${{1}}{REDACTION}"))
            .into_owned()
    }
}

// ---------------------------------------------------------------------------
// Live HTTP implementation (CLI-only construction)
// ---------------------------------------------------------------------------

/// reqwest-backed live transport. Holds the bearer header privately; it is
/// never rendered by `Debug` and every derived message is redacted upstream.
pub struct ReqwestFleetHttp {
    inner: reqwest::Client,
    base_url: String,
    auth_header: String,
}

impl std::fmt::Debug for ReqwestFleetHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestFleetHttp")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl ReqwestFleetHttp {
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        if base_url.trim().is_empty() || !base_url.starts_with("https://") {
            bail!("live fleet client requires an https:// API base URL");
        }
        if token.trim().is_empty() {
            bail!("live fleet client requires a non-empty GitHub token");
        }
        let inner = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
            .build()
            .context("building live fleet HTTP client")?;
        Ok(Self {
            inner,
            base_url: base_url.trim_end_matches('/').to_owned(),
            auth_header: format!("Bearer {token}"),
        })
    }
}

impl FleetHttp for ReqwestFleetHttp {
    async fn execute(
        &self,
        request: FleetHttpRequest,
    ) -> Result<FleetHttpResponse, FleetHttpError> {
        let mut url = format!("{}{}", self.base_url, request.url_path);
        if !request.query.is_empty() {
            let pairs = request
                .query
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&pairs);
        }
        let mut builder = match request.method {
            FleetHttpMethod::Get => self.inner.get(&url),
            FleetHttpMethod::Patch => self.inner.patch(&url),
            FleetHttpMethod::Put => self.inner.put(&url),
        };
        builder = builder
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "velnor-tools-fleet-policy")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION);
        if let Some(body) = &request.json_body {
            builder = builder.json(body);
        }
        let response = builder.send().await.map_err(|error| FleetHttpError {
            message: error.to_string(),
        })?;
        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        for name in ["retry-after", "x-ratelimit-remaining"] {
            if let Some(value) = response.headers().get(name)
                && let Ok(text) = value.to_str()
            {
                headers.insert(name.to_owned(), text.to_owned());
            }
        }
        // 204 No Content and empty bodies decode to Null instead of failing.
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(FleetHttpResponse {
            status,
            headers,
            body,
        })
    }
}

// ---------------------------------------------------------------------------
// Typed API response models
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RunnerGroupPage {
    total_count: i64,
    #[serde(default)]
    runner_groups: Vec<RunnerGroupSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerGroupSummary {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerGroupDetail {
    #[allow(dead_code)]
    pub id: i64,
    pub name: String,
    pub default: bool,
    pub inherited: bool,
    pub allows_public_repositories: bool,
    pub restricted_to_workflows: bool,
    pub workflow_restrictions_read_only: bool,
    pub visibility: String,
    pub selected_workflows: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GroupRepositoryPage {
    total_count: i64,
    #[serde(default)]
    repositories: Vec<GroupRepository>,
}

#[derive(Debug, Clone, Deserialize)]
struct GroupRepository {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct RepositoryIdentity {
    id: i64,
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub pause_between_attempts: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_ATTEMPTS,
            pause_between_attempts: PAUSE_BETWEEN_ATTEMPTS,
        }
    }
}

/// Guard flags that must fail closed before any mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupGuards {
    pub default_group: bool,
    pub inherited: bool,
    pub workflow_restrictions_read_only: bool,
}

impl GroupGuards {
    fn violations(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.default_group {
            lines.push(
                "group_guards: resolved group is the Default runner group; \
                 refusing to mutate the default group"
                    .to_owned(),
            );
        }
        if self.inherited {
            lines.push(
                "group_guards: group is inherited from an enterprise; \
                 stop and reconcile at the enterprise owner level"
                    .to_owned(),
            );
        }
        if self.workflow_restrictions_read_only {
            lines.push(
                "group_guards: workflow_restrictions_read_only=true; \
                 stop and name the enterprise owner who manages restrictions"
                    .to_owned(),
            );
        }
        lines
    }
}

/// Ordered record of one completed apply stage, safe to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStage {
    RestrictionApplied { workflows: usize },
    RepositoriesApplied { count: usize },
    FinalAuditClean,
}

impl std::fmt::Display for ApplyStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyStage::RestrictionApplied { workflows } => {
                write!(f, "workflow restriction applied ({workflows} workflows)")
            }
            ApplyStage::RepositoriesApplied { count } => {
                write!(f, "selected repository set replaced ({count} repositories)")
            }
            ApplyStage::FinalAuditClean => write!(f, "final audit clean"),
        }
    }
}

/// Typed org runner-group operations over any [`FleetHttp`] implementation.
/// All surfaced errors are secret-redacted before escaping this type.
pub struct FleetGateway<'a, H: FleetHttp> {
    http: &'a H,
    secrets: Secrets,
    retry: RetryPolicy,
}

impl<'a, H: FleetHttp> FleetGateway<'a, H> {
    pub fn new(http: &'a H, token: &str, retry: RetryPolicy) -> Self {
        Self {
            http,
            secrets: Secrets::new(token),
            retry,
        }
    }

    async fn execute_with_retry(&self, request: FleetHttpRequest) -> Result<FleetHttpResponse> {
        let mut attempt: u32 = 0;
        loop {
            let response = self
                .http
                .execute(request.clone())
                .await
                .map_err(|error| anyhow!(self.secrets.redact(&error.message)))?;
            let rate_limited =
                response.status == 403 && response.header("x-ratelimit-remaining") == Some("0");
            let transient = matches!(response.status, 429 | 502 | 503 | 504);
            if (rate_limited || transient) && attempt + 1 < self.retry.max_attempts.max(1) {
                attempt += 1;
                let retry_after = response
                    .header("retry-after")
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or_default();
                tokio::time::sleep(
                    retry_after
                        .min(MAX_RATE_LIMIT_PAUSE)
                        .max(self.retry.pause_between_attempts),
                )
                .await;
                continue;
            }
            return Ok(response);
        }
    }

    async fn send_json(
        &self,
        method: FleetHttpMethod,
        url_path: String,
        query: Vec<(String, String)>,
        json_body: Option<Value>,
        what: &str,
    ) -> Result<Value> {
        let request = FleetHttpRequest {
            method,
            url_path,
            query,
            json_body,
        };
        let response = self
            .execute_with_retry(request)
            .await
            .with_context(|| format!("requesting {what}"))?;
        if !(200..300).contains(&response.status) {
            let detail = bounded_detail(&self.secrets.redact(&response.body.to_string()));
            bail!(
                "{what} returned HTTP {} with body: {detail}",
                response.status
            );
        }
        Ok(response.body)
    }

    fn groups_path(org: &str) -> String {
        format!("/orgs/{org}/actions/runner-groups")
    }

    fn group_path(org: &str, group_id: i64) -> String {
        format!("/orgs/{org}/actions/runner-groups/{group_id}")
    }

    fn repositories_path(org: &str, group_id: i64) -> String {
        format!("{}/repositories", Self::group_path(org, group_id))
    }

    async fn list_group_summaries(&self, org: &str) -> Result<Vec<RunnerGroupSummary>> {
        let mut collected = Vec::new();
        for page in 1..=MAX_PAGES {
            let body = self
                .send_json(
                    FleetHttpMethod::Get,
                    Self::groups_path(org),
                    vec![
                        ("per_page".to_owned(), PER_PAGE.to_string()),
                        ("page".to_owned(), page.to_string()),
                    ],
                    None,
                    &format!("runner group list page {page} for '{org}'"),
                )
                .await?;
            let parsed: RunnerGroupPage = serde_json::from_value(body)
                .with_context(|| format!("parsing runner group list page {page} for '{org}'"))?;
            let total = parsed.total_count;
            let page_len = parsed.runner_groups.len();
            collected.extend(parsed.runner_groups);
            if (collected.len() as i64) >= total || page_len == 0 {
                break;
            }
        }
        Ok(collected)
    }

    /// Resolve the unique target group by stable name. Duplicate names fail
    /// closed; a missing group returns `Ok(None)`.
    pub async fn find_group(
        &self,
        organization: &str,
        group_name: &str,
    ) -> Result<Option<RunnerGroupSummary>> {
        let summaries = self.list_group_summaries(organization).await?;
        let matches: Vec<RunnerGroupSummary> = summaries
            .into_iter()
            .filter(|summary| summary.name == group_name)
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [single] => Ok(Some(single.clone())),
            _ => bail!(
                "field 'group_name': duplicate runner groups named '{group_name}' in '{organization}'; refusing to guess"
            ),
        }
    }

    pub async fn read_group_detail(
        &self,
        organization: &str,
        group_id: i64,
    ) -> Result<RunnerGroupDetail> {
        let body = self
            .send_json(
                FleetHttpMethod::Get,
                Self::group_path(organization, group_id),
                Vec::new(),
                None,
                &format!("runner group detail {group_id} for '{organization}'"),
            )
            .await?;
        serde_json::from_value(body)
            .with_context(|| format!("parsing runner group detail {group_id}"))
    }

    pub fn guard_flags(detail: &RunnerGroupDetail) -> GroupGuards {
        GroupGuards {
            default_group: detail.default,
            inherited: detail.inherited,
            workflow_restrictions_read_only: detail.workflow_restrictions_read_only,
        }
    }

    async fn list_selected_repositories(
        &self,
        organization: &str,
        group_id: i64,
    ) -> Result<BTreeSet<String>> {
        let mut collected: BTreeSet<String> = BTreeSet::new();
        for page in 1..=MAX_PAGES {
            let body = self
                .send_json(
                    FleetHttpMethod::Get,
                    Self::repositories_path(organization, group_id),
                    vec![
                        ("per_page".to_owned(), PER_PAGE.to_string()),
                        ("page".to_owned(), page.to_string()),
                    ],
                    None,
                    &format!("selected repositories page {page} of group {group_id}"),
                )
                .await?;
            let parsed: GroupRepositoryPage = serde_json::from_value(body)
                .with_context(|| format!("parsing repository page {page} of group {group_id}"))?;
            let total = parsed.total_count;
            let page_len = parsed.repositories.len();
            collected.extend(parsed.repositories.into_iter().map(|repo| repo.full_name));
            if (collected.len() as i64) >= total || page_len == 0 {
                break;
            }
        }
        Ok(collected)
    }

    async fn resolve_repository_id(&self, slug: &str) -> Result<i64> {
        let body = self
            .send_json(
                FleetHttpMethod::Get,
                format!("/repos/{slug}"),
                Vec::new(),
                None,
                &format!("repository identity for '{slug}'"),
            )
            .await?;
        let identity: RepositoryIdentity = serde_json::from_value(body)
            .with_context(|| format!("parsing repository identity for '{slug}'"))?;
        Ok(identity.id)
    }

    /// Composite observation used by audit and final verification. Missing
    /// group yields `Ok(None)`; malformed readback fails closed.
    pub async fn observe_group(
        &self,
        organization: &str,
        group_name: &str,
    ) -> Result<Option<ObservedGroup>> {
        let Some(summary) = self.find_group(organization, group_name).await? else {
            return Ok(None);
        };
        let detail = self.read_group_detail(organization, summary.id).await?;
        let repositories = self
            .list_selected_repositories(organization, summary.id)
            .await?;
        let selected_workflows: BTreeSet<String> = detail.selected_workflows.into_iter().collect();
        Ok(Some(ObservedGroup {
            name: detail.name,
            visibility: detail.visibility,
            allows_public_repositories: detail.allows_public_repositories,
            restricted_to_workflows: detail.restricted_to_workflows,
            selected_repositories: repositories,
            selected_workflows,
        }))
    }

    /// Read-only drift report against desired policy. Never mutates; drift is
    /// returned as data, only unreadable state errors.
    pub async fn audit(&self, policy: &OrgPolicy, organization: &str) -> Result<Vec<String>> {
        let group_name = policy.group_name.as_str();
        let Some(summary) = self.find_group(organization, group_name).await? else {
            return Ok(vec![format!(
                "group_name: want '{group_name}' got '<missing>'"
            )]);
        };
        let detail = self.read_group_detail(organization, summary.id).await?;
        let mut lines = Self::guard_flags(&detail).violations();
        let observed = self.observe_group(organization, group_name).await?;
        if let Some(observed) = observed {
            lines.extend(semantic_diff(policy, organization, &observed)?);
        }
        Ok(lines)
    }

    async fn patch_group_restriction(
        &self,
        organization: &str,
        group_id: i64,
        policy: &OrgPolicy,
    ) -> Result<()> {
        let body = json!({
            "name": policy.group_name,
            "visibility": policy.visibility.to_string(),
            "allows_public_repositories": policy.allows_public_repositories,
            "restricted_to_workflows": policy.restricted_to_workflows,
            "selected_workflows": policy
                .selected_workflows
                .iter()
                .map(WorkflowIdentity::to_selected_string)
                .collect::<Vec<_>>(),
        });
        self.send_json(
            FleetHttpMethod::Patch,
            Self::group_path(organization, group_id),
            Vec::new(),
            Some(body),
            &format!("workflow restriction update for group {group_id}"),
        )
        .await?;
        Ok(())
    }

    async fn put_selected_repositories(
        &self,
        organization: &str,
        group_id: i64,
        slugs: &[String],
    ) -> Result<()> {
        let mut ids = Vec::with_capacity(slugs.len());
        for slug in slugs {
            ids.push(self.resolve_repository_id(slug).await?);
        }
        self.send_json(
            FleetHttpMethod::Put,
            Self::repositories_path(organization, group_id),
            Vec::new(),
            Some(json!({ "selected_repository_ids": ids })),
            &format!("selected repository replacement for group {group_id}"),
        )
        .await?;
        Ok(())
    }

    /// Readback equality for the restriction dimension only (the repository
    /// set may legitimately still be unapplied at this point).
    fn restriction_readback_diff(detail: &RunnerGroupDetail, policy: &OrgPolicy) -> Vec<String> {
        let mut lines = Vec::new();
        let visibility = detail.visibility.clone();
        if visibility != policy.visibility.to_string() {
            lines.push(format!(
                "visibility: want '{}' got '{visibility}'",
                policy.visibility
            ));
        }
        if detail.allows_public_repositories != policy.allows_public_repositories {
            lines.push(format!(
                "allows_public_repositories: want '{}' got '{}'",
                policy.allows_public_repositories, detail.allows_public_repositories
            ));
        }
        if detail.restricted_to_workflows != policy.restricted_to_workflows {
            lines.push(format!(
                "restricted_to_workflows: want '{}' got '{}'",
                policy.restricted_to_workflows, detail.restricted_to_workflows
            ));
        }
        let got: BTreeSet<String> = detail.selected_workflows.iter().cloned().collect();
        let want: BTreeSet<String> = policy
            .selected_workflows
            .iter()
            .map(WorkflowIdentity::to_selected_string)
            .collect();
        for missing in want.difference(&got) {
            lines.push(format!("selected_workflows: missing '{missing}'"));
        }
        for extra in got.difference(&want) {
            lines.push(format!("selected_workflows: unexpected '{extra}'"));
        }
        lines
    }

    /// Digest-gated apply: restriction PATCH first, exact repository-set PUT
    /// second, readback equality after each mutation, whole-policy semantic
    /// diff last. Any disagreement aborts remaining steps fail-closed.
    pub async fn apply_reviewed_policy(
        &self,
        policy: &OrgPolicy,
        organization: &str,
        plan_digest: &str,
    ) -> Result<Vec<ApplyStage>> {
        // Digest gate: strictly before any HTTP call.
        verify_plan_digest(plan_digest, policy.digest()?.as_str())?;
        if organization != policy.organization {
            bail!(
                "field 'organization': requested '{organization}' but policy targets '{}'; refusing to mutate",
                policy.organization
            );
        }

        let Some(summary) = self.find_group(organization, &policy.group_name).await? else {
            bail!(
                "fleet apply: group '{}' missing in '{organization}'; create it manually before apply",
                policy.group_name
            );
        };
        let detail = self.read_group_detail(organization, summary.id).await?;
        let violations = Self::guard_flags(&detail).violations();
        if !violations.is_empty() {
            bail!(
                "fleet apply refused before mutation: {}",
                violations.join("; ")
            );
        }

        // Step 1: restriction first.
        self.patch_group_restriction(organization, summary.id, policy)
            .await?;
        let reread = self.read_group_detail(organization, summary.id).await?;
        let disagreement = Self::restriction_readback_diff(&reread, policy);
        if !disagreement.is_empty() {
            bail!(
                "fleet apply aborted after restriction step (repository replacement NOT attempted): readback disagreement: {}",
                disagreement.join("; ")
            );
        }

        // Step 2: exact repository-set replace.
        self.put_selected_repositories(organization, summary.id, &policy.selected_repositories)
            .await?;
        let repos = self
            .list_selected_repositories(organization, summary.id)
            .await?;
        if repos != policy.selected_repositories.iter().cloned().collect() {
            bail!(
                "fleet apply aborted after repository step: repository readback disagreement: want {:?} got {:?}",
                policy.selected_repositories,
                repos
            );
        }

        // Step 3: whole-policy semantic equality.
        let observed = self
            .observe_group(organization, &policy.group_name)
            .await?
            .ok_or_else(|| anyhow!("fleet apply: group vanished during final audit"))?;
        let diffs = semantic_diff(policy, organization, &observed)?;
        if !diffs.is_empty() {
            bail!(
                "fleet apply aborted: final audit found residual drift: {}",
                diffs.join("; ")
            );
        }
        Ok(vec![
            ApplyStage::RestrictionApplied {
                workflows: policy.selected_workflows.len(),
            },
            ApplyStage::RepositoriesApplied {
                count: policy.selected_repositories.len(),
            },
            ApplyStage::FinalAuditClean,
        ])
    }
}

fn bounded_detail(detail: &str) -> String {
    if detail.chars().count() <= ERROR_DETAIL_LIMIT {
        detail.to_owned()
    } else {
        let truncated: String = detail.chars().take(ERROR_DETAIL_LIMIT).collect();
        format!("{truncated}…[truncated]")
    }
}

// ---------------------------------------------------------------------------
// Fake API (tests only): in-memory GitHub with a recorded call log
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fake_api {
    use super::{FleetHttp, FleetHttpError, FleetHttpRequest, FleetHttpResponse};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    pub const TOKEN: &str = "ghs_fake0123456789abcdefghij";

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Divergence {
        Restriction,
        Repositories,
    }

    #[derive(Default)]
    pub struct FakeState {
        pub groups_pages: Vec<Vec<Value>>,
        pub group_detail: Value,
        /// Status code served for `GET .../runner-groups/{id}`.
        pub group_detail_status: u16,
        pub repo_pages: Vec<Vec<Value>>,
        pub repo_ids: BTreeMap<String, i64>,
        pub rate_limit_remaining: u32,
        pub fail_repo_put_remaining: u32,
        pub divergence: Option<Divergence>,
        /// Ordered `(method, path)` log of every executed request.
        pub calls: Vec<(String, String)>,
    }

    impl FakeState {
        fn total_groups(&self) -> i64 {
            self.groups_pages.iter().map(Vec::len).sum::<usize>() as i64
        }

        fn total_repos(&self) -> i64 {
            self.repo_pages.iter().map(Vec::len).sum::<usize>() as i64
        }
    }

    pub struct FakeApi(Mutex<FakeState>);

    impl FakeApi {
        pub fn new(state: FakeState) -> Self {
            Self(Mutex::new(state))
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.0.lock().expect("fake state lock")
        }

        pub fn calls(&self) -> Vec<(String, String)> {
            self.lock().calls.clone()
        }

        /// Only mutating requests, proving exactly what touched GitHub.
        pub fn mutation_calls(&self) -> Vec<(String, String)> {
            self.calls()
                .into_iter()
                .filter(|(method, _)| method == "PATCH" || method == "PUT")
                .collect()
        }

        fn respond(status: u16, body: Value) -> Result<FleetHttpResponse, FleetHttpError> {
            Ok(FleetHttpResponse {
                status,
                headers: BTreeMap::new(),
                body,
            })
        }

        fn rate_limited() -> Result<FleetHttpResponse, FleetHttpError> {
            let mut headers = BTreeMap::new();
            headers.insert("x-ratelimit-remaining".to_owned(), "0".to_owned());
            headers.insert("retry-after".to_owned(), "0".to_owned());
            Ok(FleetHttpResponse {
                status: 403,
                headers,
                body: json!({ "message": "API rate limit exceeded" }),
            })
        }
    }

    impl FleetHttp for FakeApi {
        async fn execute(
            &self,
            request: FleetHttpRequest,
        ) -> Result<FleetHttpResponse, FleetHttpError> {
            let mut state = self.lock();
            state
                .calls
                .push((request.method.as_str().to_owned(), request.url_path.clone()));

            if state.rate_limit_remaining > 0 {
                state.rate_limit_remaining -= 1;
                return Self::rate_limited();
            }

            let segments: Vec<&str> = request
                .url_path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            let page_index = request
                .query
                .iter()
                .find(|(key, _)| key == "page")
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let body = request.json_body.clone();

            // GET /repos/{owner}/{repo}
            if segments.first() == Some(&"repos") && segments.len() == 3 {
                let slug = format!("{}/{}", segments[1], segments[2]);
                return match state.repo_ids.get(&slug) {
                    Some(id) => Self::respond(200, json!({ "id": id, "full_name": slug })),
                    None => Self::respond(404, json!({ "message": "Not Found" })),
                };
            }

            if segments.first() != Some(&"orgs")
                || segments.get(2) != Some(&"actions")
                || segments.get(3) != Some(&"runner-groups")
            {
                return Self::respond(404, json!({ "message": "unknown route" }));
            }

            // Segments after /orgs/{org}/actions/runner-groups:
            // 0 = list, 1 = group detail, 2 = group sub-resource.
            match (request.method.as_str(), segments.len() - 4) {
                ("GET", 0) => {
                    let groups = state
                        .groups_pages
                        .get(page_index)
                        .cloned()
                        .unwrap_or_default();
                    let total = state.total_groups();
                    Self::respond(
                        200,
                        json!({ "total_count": total, "runner_groups": groups }),
                    )
                }
                ("GET", 1) => {
                    let status = if state.group_detail_status == 0 {
                        200
                    } else {
                        state.group_detail_status
                    };
                    Self::respond(status, state.group_detail.clone())
                }
                ("PATCH", 1) => {
                    let Some(body) = body else {
                        return Self::respond(400, json!({ "message": "missing body" }));
                    };
                    let workflows: Vec<String> =
                        if state.divergence == Some(Divergence::Restriction) {
                            vec![
                                "tailrocks/ruxel/.github/workflows/other.yml@refs/heads/main"
                                    .to_owned(),
                            ]
                        } else {
                            body["selected_workflows"]
                                .as_array()
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default()
                        };
                    let detail = &mut state.group_detail;
                    detail["name"] = body["name"].clone();
                    detail["visibility"] = body["visibility"].clone();
                    detail["allows_public_repositories"] =
                        body["allows_public_repositories"].clone();
                    detail["restricted_to_workflows"] = body["restricted_to_workflows"].clone();
                    detail["selected_workflows"] = json!(workflows);
                    Self::respond(200, detail.clone())
                }
                ("GET", 2) if segments[5] == "repositories" => {
                    let page = state
                        .repo_pages
                        .get(page_index)
                        .cloned()
                        .unwrap_or_default();
                    let total = state.total_repos();
                    Self::respond(200, json!({ "total_count": total, "repositories": page }))
                }
                ("PUT", 2) if segments[5] == "repositories" => {
                    if state.fail_repo_put_remaining > 0 {
                        state.fail_repo_put_remaining -= 1;
                        return Self::respond(
                            500,
                            json!({ "message": format!("internal error while handling {}", TOKEN) }),
                        );
                    }
                    let Some(body) = body else {
                        return Self::respond(400, json!({ "message": "missing body" }));
                    };
                    let ids: Vec<i64> = body["selected_repository_ids"]
                        .as_array()
                        .map(|values| values.iter().filter_map(Value::as_i64).collect())
                        .unwrap_or_default();
                    let mut all = ids;
                    if state.divergence == Some(Divergence::Repositories) {
                        all.push(99_999);
                    }
                    let pages: Vec<Vec<Value>> = all
                        .chunks(2)
                        .map(|chunk| {
                            chunk
                                .iter()
                                .filter_map(|id| {
                                    state
                                        .repo_ids
                                        .iter()
                                        .find(|(_, resolved)| **resolved == *id)
                                        .map(|(slug, _)| json!({ "id": id, "full_name": slug }))
                                })
                                .collect()
                        })
                        .collect();
                    state.repo_pages = pages;
                    Self::respond(204, Value::Null)
                }
                _ => Self::respond(404, json!({ "message": "unknown route" })),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: fake-API proof matrix (no real network anywhere)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_policy::GroupVisibility;
    use fake_api::{Divergence, FakeApi, FakeState, TOKEN};
    use serde_json::json;

    const ORG: &str = "tailrocks";

    fn desired_policy() -> OrgPolicy {
        OrgPolicy {
            organization: ORG.to_owned(),
            visibility: GroupVisibility::Selected,
            selected_repositories: vec![
                "tailrocks/ruxel".to_owned(),
                "tailrocks/schemalane".to_owned(),
            ],
            selected_workflows: vec![
                WorkflowIdentity {
                    path: "tailrocks/ruxel/.github/workflows/ci.yml".to_owned(),
                    git_ref: "refs/heads/main".to_owned(),
                },
                WorkflowIdentity {
                    path: "tailrocks/schemalane/.github/workflows/ci.yml".to_owned(),
                    git_ref: "refs/heads/trunk".to_owned(),
                },
            ],
            ..OrgPolicy::new(ORG)
        }
    }

    fn group_summary(id: i64, name: &str) -> serde_json::Value {
        json!({ "id": id, "name": name, "default": false, "inherited": false })
    }

    fn group_detail_json() -> serde_json::Value {
        json!({
            "id": 7,
            "name": "velnor-trusted",
            "default": false,
            "inherited": false,
            "visibility": "selected",
            "allows_public_repositories": false,
            "restricted_to_workflows": false,
            "workflow_restrictions_read_only": false,
            "selected_workflows": []
        })
    }

    fn base_state(policy: &OrgPolicy) -> FakeState {
        let mut repo_ids = BTreeMap::new();
        let mut repos = Vec::new();
        for (index, slug) in policy.selected_repositories.iter().enumerate() {
            let id = 11 + index as i64;
            repo_ids.insert(slug.clone(), id);
            repos.push(json!({ "id": id, "full_name": slug }));
        }
        FakeState {
            groups_pages: vec![vec![group_summary(7, "velnor-trusted")]],
            group_detail: group_detail_json(),
            repo_pages: vec![repos],
            repo_ids,
            ..FakeState::default()
        }
    }

    async fn audit_lines(api: &FakeApi, policy: &OrgPolicy) -> Vec<String> {
        FleetGateway::new(api, TOKEN, RetryPolicy::default())
            .audit(policy, ORG)
            .await
            .expect("audit runs")
    }

    async fn apply_ok(api: &FakeApi, policy: &OrgPolicy) -> Vec<ApplyStage> {
        let digest = policy.digest().expect("digest");
        FleetGateway::new(api, TOKEN, RetryPolicy::default())
            .apply_reviewed_policy(policy, ORG, &digest)
            .await
            .expect("apply succeeds")
    }

    #[tokio::test]
    async fn pagination_collects_exact_sets_across_pages() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.groups_pages = vec![
            vec![group_summary(1, "Default")],
            vec![group_summary(2, "beta-group")],
            vec![group_summary(7, "velnor-trusted")],
        ];
        state.repo_pages = policy
            .selected_repositories
            .iter()
            .enumerate()
            .map(|(index, slug)| vec![json!({ "id": 11 + index as i64, "full_name": slug })])
            .collect();
        let api = FakeApi::new(state);

        let observed = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .observe_group(ORG, "velnor-trusted")
            .await
            .expect("observe")
            .expect("group present");
        assert_eq!(
            observed.selected_repositories,
            policy.selected_repositories.iter().cloned().collect()
        );
        let list_calls = api
            .calls()
            .iter()
            .filter(|(method, _)| method == "GET")
            .filter(|(_, path)| path == "/orgs/tailrocks/actions/runner-groups")
            .count();
        assert_eq!(list_calls, 3, "three paginated group-list pages");
    }

    #[tokio::test]
    async fn duplicate_group_names_rejected_before_any_mutation() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.groups_pages = vec![vec![
            group_summary(7, "velnor-trusted"),
            group_summary(8, "velnor-trusted"),
        ]];
        let api = FakeApi::new(state);

        let err = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .audit(&policy, ORG)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("duplicate runner groups named 'velnor-trusted'"),
            "{err}"
        );
        assert!(api.mutation_calls().is_empty());
    }

    #[tokio::test]
    async fn missing_group_reports_one_class_and_apply_aborts_without_mutation() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.groups_pages = vec![vec![group_summary(1, "Default")]];
        let api = FakeApi::new(state);

        let lines = audit_lines(&api, &policy).await;
        assert_eq!(
            lines,
            vec!["group_name: want 'velnor-trusted' got '<missing>'".to_owned()]
        );

        let digest = policy.digest().expect("digest");
        let gateway = FleetGateway::new(&api, TOKEN, RetryPolicy::default());
        let err = gateway
            .apply_reviewed_policy(&policy, ORG, &digest)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing"), "{err}");
        assert!(
            api.mutation_calls().is_empty(),
            "no mutation without the group"
        );
    }

    #[tokio::test]
    async fn inherited_readonly_and_default_groups_fail_closed() {
        let policy = desired_policy();
        for (field, marker) in [
            ("inherited", "inherited from an enterprise"),
            (
                "workflow_restrictions_read_only",
                "workflow_restrictions_read_only=true",
            ),
            ("default", "Default runner group"),
        ] {
            let mut state = base_state(&policy);
            state.group_detail[field] = json!(true);
            let api = FakeApi::new(state);

            let lines = audit_lines(&api, &policy).await;
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with("group_guards:") && line.contains(marker)),
                "{field} guard not reported: {lines:?}"
            );

            let digest = policy.digest().expect("digest");
            let gateway = FleetGateway::new(&api, TOKEN, RetryPolicy::default());
            let err = gateway
                .apply_reviewed_policy(&policy, ORG, &digest)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("refused before mutation"), "{err}");
            assert!(
                api.mutation_calls().is_empty(),
                "{field} blocks all mutations"
            );
        }
    }

    #[tokio::test]
    async fn omitted_group_policy_readback_fields_fail_before_mutation() {
        let policy = desired_policy();
        let digest = policy.digest().expect("digest");
        for field in [
            "allows_public_repositories",
            "restricted_to_workflows",
            "visibility",
            "selected_workflows",
        ] {
            let mut state = base_state(&policy);
            state
                .group_detail
                .as_object_mut()
                .expect("group detail object")
                .remove(field);
            let api = FakeApi::new(state);

            let result = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
                .apply_reviewed_policy(&policy, ORG, &digest)
                .await;
            assert!(
                result.is_err(),
                "omitted {field} unexpectedly allowed apply: {result:?}"
            );
            let err = result.expect_err("missing readback field must fail closed");
            let error_chain = format!("{err:#}");
            assert!(
                error_chain.contains(&format!("missing field `{field}`")),
                "{error_chain}"
            );
            assert!(
                api.mutation_calls().is_empty(),
                "omitted {field} must fail before PATCH/PUT: {error_chain}"
            );
        }
    }

    #[tokio::test]
    async fn empty_workflow_list_lists_every_missing_identity() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.group_detail["selected_workflows"] = json!([]);
        let api = FakeApi::new(state);

        let lines = audit_lines(&api, &policy).await;
        for identity in &policy.selected_workflows {
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("selected_workflows: missing")
                        && line.contains(identity.path.as_str())),
                "missing identity {} not listed: {lines:?}",
                identity.path
            );
        }
        assert!(
            !lines.iter().any(|line| line.contains("unexpected")),
            "nothing unexpected with empty readback: {lines:?}"
        );
    }

    #[tokio::test]
    async fn broader_than_desired_set_listed_unexpected() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state
            .repo_pages
            .push(vec![json!({ "id": 42, "full_name": "tailrocks/termrock" })]);
        let api = FakeApi::new(state);

        let lines = audit_lines(&api, &policy).await;
        assert!(
            lines
                .iter()
                .any(|line| line.contains("unexpected 'tailrocks/termrock'")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn narrower_than_desired_set_listed_missing() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.repo_pages[0].remove(1); // schemalane absent from live selection
        let api = FakeApi::new(state);

        let lines = audit_lines(&api, &policy).await;
        assert!(
            lines
                .iter()
                .any(|line| line.contains("missing 'tailrocks/schemalane'")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn stale_digest_and_wrong_org_refused_before_any_http_call() {
        let policy = desired_policy();
        let api = FakeApi::new(base_state(&policy));
        let gateway = FleetGateway::new(&api, TOKEN, RetryPolicy::default());

        let actual_digest = policy.digest().expect("digest");
        let err = gateway
            .apply_reviewed_policy(&policy, ORG, "sha256:stale0000")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("stale"), "{err}");
        assert!(err.contains("sha256:stale0000") && err.contains(actual_digest.as_str()));
        assert!(
            api.calls().is_empty(),
            "digest gate precedes every HTTP call"
        );

        let err = gateway
            .apply_reviewed_policy(&policy, "chainargos", actual_digest.as_str())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requested 'chainargos'"), "{err}");
        assert!(api.calls().is_empty(), "org gate precedes every HTTP call");
    }

    #[tokio::test]
    async fn partial_failure_mid_apply_documents_state_and_never_continues() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.fail_repo_put_remaining = 1; // repository PUT answers 500 once (terminal)
        let api = FakeApi::new(state);

        let digest = policy.digest().expect("digest");
        let err = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .apply_reviewed_policy(&policy, ORG, &digest)
            .await
            .unwrap_err()
            .to_string();

        // Documented failure point + redacted embedded token.
        assert!(
            err.contains("returned HTTP 500") && err.contains(REDACTION) && !err.contains(TOKEN),
            "{err}"
        );

        let calls = api.calls();
        let patch_seen = calls
            .iter()
            .any(|(method, path)| method == "PATCH" && path.ends_with("/runner-groups/7"));
        let put_attempts = calls
            .iter()
            .filter(|(method, path)| method == "PUT" && path.ends_with("/repositories"))
            .count();
        assert!(patch_seen, "restriction step completed first: {calls:?}");
        assert_eq!(
            put_attempts, 1,
            "failed PUT is terminal, never retried or repeated"
        );
        let last_is_put = calls.last().is_some_and(|(method, _)| method == "PUT");
        assert!(
            last_is_put,
            "no silent continue after the failed step: {calls:?}"
        );
    }

    #[tokio::test]
    async fn rate_limit_retry_succeeds_then_exhaustion_fails_redacted() {
        // One rate-limited response, then the API recovers: apply completes.
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.rate_limit_remaining = 1;
        let api = FakeApi::new(state);
        let stages = apply_ok(&api, &policy).await;
        assert_eq!(stages.last(), Some(&ApplyStage::FinalAuditClean));

        // Persistent rate limiting exhausts bounded attempts and fails closed.
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.rate_limit_remaining = u32::MAX;
        let api = FakeApi::new(state);
        let digest = policy.digest().expect("digest");
        let err = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .apply_reviewed_policy(&policy, ORG, &digest)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 403"), "{err}");
        assert!(!err.contains(TOKEN), "token leaked: {err}");
        assert!(
            api.mutation_calls().is_empty(),
            "no mutation before a readable API"
        );
    }

    #[tokio::test]
    async fn errors_surface_redacted_without_raw_tokens() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.group_detail_status = 500;
        state.group_detail = json!({
            "message": format!("suspicious echo bearer {} done", TOKEN)
        });
        let api = FakeApi::new(state);

        let err = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .read_group_detail(ORG, 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains(TOKEN), "raw token surfaced: {err}");
        assert!(err.contains(REDACTION), "redaction marker present: {err}");

        // Bearer-form scrubbing works even for unregistered values.
        let secrets = Secrets::new("unused");
        let text =
            secrets.redact("Authorization: Bearer abcdef.012345 and ghx_unknownvalue1234567890");
        assert!(!text.contains("abcdef.012345"), "{text}");
        assert!(text.contains(REDACTION), "{text}");
    }

    #[tokio::test]
    async fn idempotent_reaudit_after_successful_apply_is_clean() {
        let policy = desired_policy();
        let api = FakeApi::new(base_state(&policy));
        let stages = apply_ok(&api, &policy).await;
        assert_eq!(
            stages,
            vec![
                ApplyStage::RestrictionApplied { workflows: 2 },
                ApplyStage::RepositoriesApplied { count: 2 },
                ApplyStage::FinalAuditClean,
            ]
        );
        // Re-audit sees zero drift; re-apply with same digest also succeeds.
        let lines = audit_lines(&api, &policy).await;
        assert!(lines.is_empty(), "residual drift after apply: {lines:?}");
        let again = apply_ok(&api, &policy).await;
        assert_eq!(again.last(), Some(&ApplyStage::FinalAuditClean));
    }

    #[tokio::test]
    async fn readback_disagreement_fails_closed_and_skips_remaining_steps() {
        let policy = desired_policy();
        let mut state = base_state(&policy);
        state.divergence = Some(Divergence::Restriction);
        let api = FakeApi::new(state);

        let digest = policy.digest().expect("digest");
        let err = FleetGateway::new(&api, TOKEN, RetryPolicy::default())
            .apply_reviewed_policy(&policy, ORG, &digest)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("aborted after restriction step")
                && err.contains("repository replacement NOT attempted")
                && err.contains("other.yml"),
            "{err}"
        );
        assert!(
            api.mutation_calls().len() == 1,
            "only the restriction PATCH ran: {:?}",
            api.mutation_calls()
        );
    }

    #[tokio::test]
    async fn apply_order_restriction_patch_precedes_repository_replace() {
        let policy = desired_policy();
        let api = FakeApi::new(base_state(&policy));
        apply_ok(&api, &policy).await;

        let calls = api.calls();
        let patch_index = calls
            .iter()
            .position(|(method, _)| method == "PATCH")
            .expect("restriction PATCH recorded");
        let put_index = calls
            .iter()
            .position(|(method, path)| method == "PUT" && path.ends_with("/repositories"))
            .expect("repository PUT recorded");
        assert!(
            patch_index < put_index,
            "restriction must precede repository replace: {calls:?}"
        );
        // Readback equality checks sit between the two mutations.
        assert!(
            calls[patch_index + 1..put_index]
                .iter()
                .any(|(method, _)| method == "GET"),
            "restriction readback happens before repository replace: {calls:?}"
        );
    }
}
