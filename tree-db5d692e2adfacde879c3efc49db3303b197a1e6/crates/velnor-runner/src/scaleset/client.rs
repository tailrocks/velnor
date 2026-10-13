//! Admin-plane scale-set client (`client.go` at [`crate::scaleset::UPSTREAM_COMMIT`]).
//!
//! Mirrors `NewClientWithGitHubApp` / `NewClientWithPersonalAccessToken` /
//! `NewClientWithJWTProvider`, the registration-token → admin-connection →
//! JWT-expiry token chain (`updateTokenIfNeeded`, same 60s skew margin), the
//! Actions Service request builder (`api-version=6.0-preview` default), and
//! the scale-set / runner CRUD surface including `GenerateJitRunnerConfig`.
//! Session traffic lives in [`crate::scaleset::session`].

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use url::Url;
use velnor_model::{
    RunnerGroup, RunnerGroupList, RunnerReference, RunnerReferenceList, RunnerScaleSet,
    RunnerScaleSetJitRunnerConfig, RunnerScaleSetJitRunnerSetting, RunnerScaleSetList,
    SCALESET_API_VERSION, SCALESET_ENDPOINT,
};

use crate::protocol::redacted_authenticated_url;
use crate::scaleset::backoff::RetryPolicy;
use crate::scaleset::config::GitHubConfig;
use crate::scaleset::credentials::{
    ActionsAuth, GitHubAppAuth, InstallationAccessToken, PemJwtProvider,
};
use crate::scaleset::errors::{
    request_response_error, trim_byte_order_mark, ScaleSetError, ScaleSetFault,
};

/// Classic agent endpoint (`runnerEndpoint`).
const RUNNER_ENDPOINT: &str = "_apis/distributedtask/pools/0/agents";
/// Runner-groups endpoint used by `GetRunnerGroupByName` (verbatim path).
const RUNNER_GROUPS_PATH: &str = "/_apis/runtime/runnergroups/";
/// Admin-connection handshake path.
const RUNNER_REGISTRATION_PATH: &str = "/actions/runner-registration";
/// Skew margin before admin-token expiry (`updateTokenIfNeeded`).
const TOKEN_REFRESH_SKEW_SECS: u64 = 60;

/// Identity block serialized into the `User-Agent` JSON (`SystemInfo`).
#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    /// Name of the scale-set implementation (`velnor`).
    pub system: String,
    /// Client version.
    pub version: String,
    /// Git commit SHA of the client.
    pub commit_sha: String,
    /// ID of the scale set.
    pub scale_set_id: i32,
    /// Subsystem (`listener`, `controller`, ...).
    pub subsystem: String,
}

/// Cached Actions Service admin token (`actionsServiceAdminToken`).
///
/// `Debug` never prints the credential: the authorization header renders
/// redacted, mirroring [`ActionsAuth`].
#[derive(Clone)]
struct AdminToken {
    authorization_header: String,
    expires_at_epoch: u64,
    url: String,
}

impl std::fmt::Debug for AdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminToken")
            .field("authorization_header", &"<redacted>")
            .field("expires_at_epoch", &self.expires_at_epoch)
            .field("url", &redacted_authenticated_url(&self.url))
            .finish()
    }
}

/// Raw HTTP response: status + headers + BOM-stripped body (`sendRequest`).
pub(crate) struct RawResponse {
    pub method: String,
    pub url: String,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("method", &self.method)
            .field("url", &redacted_authenticated_url(&self.url))
            .field("status", &self.status)
            .field("headers", &"<redacted>")
            .field("body", &"<redacted>")
            .finish()
    }
}

impl RawResponse {
    pub(crate) fn failed(&self, fault: Option<ScaleSetFault>, detail: &str) -> ScaleSetError {
        request_response_error(
            &self.method,
            &self.url,
            self.status,
            &self.headers,
            &self.body,
            fault,
            detail,
        )
    }

    pub(crate) fn decode<T: serde::de::DeserializeOwned>(
        &self,
        what: &str,
    ) -> Result<T, ScaleSetError> {
        serde_json::from_slice::<T>(trim_byte_order_mark(&self.body))
            .map_err(|error| self.failed(None, &format!("failed to decode {what}: {error}")))
    }
}

struct ClientInner {
    http: Client,
    config: GitHubConfig,
    auth: ActionsAuth,
    retry: RetryPolicy,
    system_info: RwLock<SystemInfo>,
    user_agent: RwLock<String>,
    admin_token: RwLock<Option<AdminToken>>,
    refresh_lock: Mutex<()>,
}

impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientInner")
            .field(
                "config_url",
                &redacted_authenticated_url(self.config.config_url.as_str()),
            )
            .field("auth", &self.auth)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

/// GitHub Actions Scale Set client (`Client`). Cheap to clone.
#[derive(Debug, Clone)]
pub struct ScaleSetClient {
    inner: Arc<ClientInner>,
}

impl ScaleSetClient {
    /// Mirror of `newClient`: parse config URL, validate credentials, build
    /// the HTTP client with the long-poll timeout.
    pub fn new(
        github_config_url: &str,
        auth: ActionsAuth,
        system_info: SystemInfo,
        retry: RetryPolicy,
    ) -> Result<Self> {
        let config = GitHubConfig::parse(github_config_url)
            .with_context(|| format!("failed to parse githubConfigURL: {github_config_url}"))?;
        auth.validate().context("invalid credentials")?;
        let http = Client::builder()
            .timeout(retry.timeout)
            .build()
            .context("build scale-set HTTP client")?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                http,
                config,
                auth,
                retry,
                system_info: RwLock::new(system_info.clone()),
                user_agent: RwLock::new(user_agent_string(&system_info)),
                admin_token: RwLock::new(None),
                refresh_lock: Mutex::new(()),
            }),
        })
    }

    /// Mirror of `NewClientWithGitHubApp`.
    pub fn new_with_app(
        github_config_url: &str,
        app: &GitHubAppAuth,
        system_info: SystemInfo,
        retry: RetryPolicy,
    ) -> Result<Self> {
        app.validate().context("invalid credentials")?;
        let provider = PemJwtProvider::new(&app.client_id, &app.private_key_pem)
            .context("invalid credentials")?;
        Self::new(
            github_config_url,
            ActionsAuth::app(Arc::new(provider), app.installation_id),
            system_info,
            retry,
        )
    }

    /// Mirror of `NewClientWithPersonalAccessToken`.
    pub fn new_with_pat(
        github_config_url: &str,
        personal_access_token: &str,
        system_info: SystemInfo,
        retry: RetryPolicy,
    ) -> Result<Self> {
        Self::new(
            github_config_url,
            ActionsAuth::pat(personal_access_token.to_string()),
            system_info,
            retry,
        )
    }

    /// Mirror of `NewClientWithJWTProvider` (KMS/HSM signing).
    pub fn new_with_jwt_provider(
        github_config_url: &str,
        installation_id: i64,
        provider: Arc<dyn crate::scaleset::JwtProvider>,
        system_info: SystemInfo,
        retry: RetryPolicy,
    ) -> Result<Self> {
        if installation_id == 0 {
            anyhow::bail!("installation ID is required");
        }
        Self::new(
            github_config_url,
            ActionsAuth::app(provider, installation_id),
            system_info,
            retry,
        )
    }

    /// Mirror of `SetSystemInfo`.
    pub async fn set_system_info(&self, info: SystemInfo) {
        *self.inner.system_info.write().await = info.clone();
        *self.inner.user_agent.write().await = user_agent_string(&info);
    }

    /// Mirror of `SystemInfo()`.
    pub async fn system_info(&self) -> SystemInfo {
        self.inner.system_info.read().await.clone()
    }

    pub(crate) async fn user_agent(&self) -> String {
        self.inner.user_agent.read().await.clone()
    }

    /// Mirror of `updateTokenIfNeeded`: cached snapshot with 60s skew margin,
    /// mutex-guarded refresh with double-checked locking.
    async fn update_token_if_needed(&self) -> Result<AdminToken, ScaleSetError> {
        if let Some(token) = self.admin_token_snapshot().await {
            return Ok(token);
        }
        let _guard = self.inner.refresh_lock.lock().await;
        if let Some(token) = self.admin_token_snapshot().await {
            return Ok(token);
        }
        let registration = self
            .get_runner_registration_token()
            .await
            .map_err(|error| error.context("failed to get runner registration token on refresh"))?;
        let connection = self
            .get_actions_service_admin_connection(&registration.token)
            .await
            .map_err(|error| {
                error.context("failed to get actions service admin connection on refresh")
            })?;
        let expires_at = admin_token_expires_at(&connection.admin_token).map_err(|error| {
            ScaleSetError::Local(format!(
                "failed to get admin token expire at on refresh: {error}"
            ))
        })?;
        let token = AdminToken {
            authorization_header: format!("Bearer {}", connection.admin_token),
            expires_at_epoch: expires_at,
            url: connection.actions_service_url,
        };
        *self.inner.admin_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Mirror of `actionsServiceAdminTokenSnapshot`.
    async fn admin_token_snapshot(&self) -> Option<AdminToken> {
        let token = self.inner.admin_token.read().await.clone()?;
        let now = unix_now().ok()?;
        if now.saturating_add(TOKEN_REFRESH_SKEW_SECS) > token.expires_at_epoch {
            return None;
        }
        Some(token)
    }

    /// Mirror of `getRunnerRegistrationToken` (expects 201).
    async fn get_runner_registration_token(&self) -> Result<RegistrationToken, ScaleSetError> {
        let path = self.inner.config.registration_token_path();
        let url = self.github_api_url(&path)?;
        let bearer = if let Some(token) = self.inner.auth.token.as_deref() {
            format!("Bearer {token}")
        } else {
            let access = self
                .fetch_access_token()
                .await
                .map_err(|error| error.context("failed to fetch access token"))?;
            format!("Bearer {}", access.token)
        };
        // Upstream sends an empty buffer as the POST body.
        let response = self
            .execute(Method::POST, url, |request| {
                request
                    .header(CONTENT_TYPE, "application/vnd.github.v3+json")
                    .header(AUTHORIZATION, bearer.clone())
                    .body(Vec::new())
            })
            .await?;
        if response.status != StatusCode::CREATED {
            return Err(response.failed(
                None,
                &format!(
                    "failed to get runner registration token ({})",
                    response.status
                ),
            ));
        }
        let token: RegistrationToken = response.decode("runner registration token")?;
        if token.token.is_empty() {
            return Err(response.failed(None, "runner registration token missing token"));
        }
        Ok(token)
    }

    /// Mirror of `fetchAccessToken` (expects 201).
    async fn fetch_access_token(&self) -> Result<InstallationAccessToken, ScaleSetError> {
        let provider = self.inner.auth.jwt_provider.clone().ok_or_else(|| {
            ScaleSetError::Local(
                "failed to get JWT for GitHub App auth: no JWT provider".to_string(),
            )
        })?;
        let jwt = provider.token().await.map_err(|error| {
            ScaleSetError::Local(format!("failed to get JWT for GitHub App auth: {error}"))
        })?;
        let path = format!(
            "/app/installations/{}/access_tokens",
            self.inner.auth.installation_id
        );
        let url = self.github_api_url(&path)?;
        let response = self
            .execute(Method::POST, url, |request| {
                request
                    .header(CONTENT_TYPE, "application/vnd.github+json")
                    .header(AUTHORIZATION, format!("Bearer {jwt}"))
            })
            .await?;
        if response.status != StatusCode::CREATED {
            return Err(response.failed(
                None,
                &format!(
                    "failed to get access token for GitHub App auth ({})",
                    response.status
                ),
            ));
        }
        response.decode("access token for GitHub App auth")
    }

    /// Mirror of `getActionsServiceAdminConnection` (expects 2xx, requires
    /// URL + token; retries 401/403 like upstream's custom `CheckRetry`).
    async fn get_actions_service_admin_connection(
        &self,
        registration_token: &str,
    ) -> Result<AdminConnection, ScaleSetError> {
        #[derive(Debug, Serialize)]
        struct HandshakeBody {
            url: String,
            runner_event: String,
        }
        // Upstream disables HTML escaping on this body.
        let body = serde_json::to_vec(&HandshakeBody {
            url: self.inner.config.config_url.to_string(),
            runner_event: "register".to_string(),
        })
        .map_err(|error| ScaleSetError::Local(format!("failed to encode body: {error}")))?;
        let url = self.github_api_url(RUNNER_REGISTRATION_PATH)?;
        let response = self
            .execute_admin_handshake(url, body, registration_token)
            .await?;
        if !response.status.is_success() {
            return Err(response.failed(
                None,
                &format!("unexpected status code: {}", response.status.as_u16()),
            ));
        }
        let connection: AdminConnectionWire =
            response.decode("actions service admin connection")?;
        match (connection.url, connection.token) {
            (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                Ok(AdminConnection {
                    actions_service_url: url,
                    admin_token: token,
                })
            }
            (url, token) => Err(ScaleSetError::Local(format!(
                "actions service admin connection missing {}",
                match (
                    url.as_deref().unwrap_or_default().is_empty(),
                    token.as_deref().unwrap_or_default().is_empty()
                ) {
                    (true, true) => "url and token",
                    (true, false) => "url",
                    _ => "token",
                }
            ))),
        }
    }

    /// Mirror of `newGitHubAPIRequest` + `newActionsServiceRequest(WithQuery)`.
    pub(crate) async fn actions_service_request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<RawResponse, ScaleSetError> {
        self.actions_service_request_inner(method, path, query, body, None)
            .await
    }

    /// Actions Service request authorized with an explicit bearer token.
    /// `AcquireJobs` routes through `newActionsServiceRequest` but swaps the
    /// admin token for the session's queue token; the admin token is still
    /// refreshed first because the request URL comes from it.
    pub(crate) async fn actions_service_request_with_bearer(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
        bearer: &str,
    ) -> Result<RawResponse, ScaleSetError> {
        self.actions_service_request_inner(
            method,
            path,
            query,
            body,
            Some(format!("Bearer {bearer}")),
        )
        .await
    }

    async fn actions_service_request_inner(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
        authorization_override: Option<String>,
    ) -> Result<RawResponse, ScaleSetError> {
        let token = self.update_token_if_needed().await?;
        let url = actions_request_url(&token.url, path, query)?;
        let authorization = authorization_override.unwrap_or(token.authorization_header.clone());
        let user_agent = self.user_agent().await;
        self.execute(method, url, move |request| {
            let request = request
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, authorization.clone())
                .header(USER_AGENT, user_agent.clone());
            if let Some(body) = body.clone() {
                request.body(body)
            } else {
                request
            }
        })
        .await
    }

    /// Mirror of `GetRunnerScaleSet` (nil → `Ok(None)` on count 0).
    pub async fn get_runner_scale_set(
        &self,
        runner_group_id: i32,
        name: &str,
    ) -> Result<Option<RunnerScaleSet>, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                SCALESET_ENDPOINT,
                &[
                    ("runnerGroupId".to_string(), runner_group_id.to_string()),
                    ("name".to_string(), name.to_string()),
                ],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        let list: RunnerScaleSetList = response.decode("runner scale set list")?;
        match list.count {
            1 => list.runner_scale_sets.into_iter().next().map_or_else(
                || Err(response.failed(None, "runner scale set list count lies about its value")),
                |set| Ok(Some(set)),
            ),
            0 => Ok(None),
            _ => Err(response.failed(
                None,
                &format!("multiple runner scale sets found with name {name:?}"),
            )),
        }
    }

    /// Mirror of `ListRunnerScaleSets`.
    pub async fn list_runner_scale_sets(
        &self,
        runner_group_id: i32,
    ) -> Result<Vec<RunnerScaleSet>, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                SCALESET_ENDPOINT,
                &[("runnerGroupId".to_string(), runner_group_id.to_string())],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response
            .decode::<RunnerScaleSetList>("runner scale set list")
            .map(|list| list.runner_scale_sets)
    }

    /// Mirror of `GetRunnerScaleSetByID`.
    pub async fn get_runner_scale_set_by_id(
        &self,
        runner_scale_set_id: i32,
    ) -> Result<RunnerScaleSet, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                &format!("/{SCALESET_ENDPOINT}/{runner_scale_set_id}"),
                &[],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response.decode("runner scale set")
    }

    /// Mirror of `CreateRunnerScaleSet` (`ensureLabels` + default types).
    pub async fn create_runner_scale_set(
        &self,
        set: &RunnerScaleSet,
    ) -> Result<RunnerScaleSet, ScaleSetError> {
        let mut set = set.clone();
        ensure_labels(&mut set)?;
        apply_default_label_types(&mut set);
        let body = serde_json::to_vec(&set).map_err(|error| {
            ScaleSetError::Local(format!("failed to marshal runner scale set: {error}"))
        })?;
        let response = self
            .actions_service_request(Method::POST, SCALESET_ENDPOINT, &[], Some(body))
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response.decode("created runner scale set")
    }

    /// Mirror of `UpdateRunnerScaleSet` (PATCH, default types).
    pub async fn update_runner_scale_set(
        &self,
        runner_scale_set_id: i32,
        set: &RunnerScaleSet,
    ) -> Result<RunnerScaleSet, ScaleSetError> {
        let mut set = set.clone();
        apply_default_label_types(&mut set);
        let body = serde_json::to_vec(&set).map_err(|error| {
            ScaleSetError::Local(format!("failed to marshal runner scale set: {error}"))
        })?;
        let response = self
            .actions_service_request(
                Method::PATCH,
                &format!("{SCALESET_ENDPOINT}/{runner_scale_set_id}"),
                &[],
                Some(body),
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response.decode("updated runner scale set")
    }

    /// Mirror of `DeleteRunnerScaleSet` (expects 204).
    pub async fn delete_runner_scale_set(
        &self,
        runner_scale_set_id: i32,
    ) -> Result<(), ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::DELETE,
                &format!("/{SCALESET_ENDPOINT}/{runner_scale_set_id}"),
                &[],
                None,
            )
            .await?;
        if response.status != StatusCode::NO_CONTENT {
            return Err(unexpected_status(&response));
        }
        Ok(())
    }

    /// Mirror of `GetRunnerGroupByName` (errors on 0 and on many).
    pub async fn get_runner_group_by_name(
        &self,
        runner_group: &str,
    ) -> Result<RunnerGroup, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                RUNNER_GROUPS_PATH,
                &[("groupName".to_string(), runner_group.to_string())],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        let list: RunnerGroupList = response.decode("runner group list")?;
        match list.count {
            1 => list.runner_groups.into_iter().next().map_or_else(
                || Err(response.failed(None, "runner group list count lies about its value")),
                Ok,
            ),
            0 => Err(response.failed(
                None,
                &format!("no runner group found with name {runner_group:?}"),
            )),
            _ => Err(response.failed(
                None,
                &format!("multiple runner group found with name {runner_group:?}"),
            )),
        }
    }

    /// Mirror of `GetRunner`.
    pub async fn get_runner(&self, runner_id: i32) -> Result<RunnerReference, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                &format!("/{RUNNER_ENDPOINT}/{runner_id}"),
                &[],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response.decode("runner reference")
    }

    /// Mirror of `GetRunnerByName` (nil → `Ok(None)` on count 0).
    pub async fn get_runner_by_name(
        &self,
        runner_name: &str,
    ) -> Result<Option<RunnerReference>, ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::GET,
                RUNNER_ENDPOINT,
                &[("agentName".to_string(), runner_name.to_string())],
                None,
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        let list: RunnerReferenceList = response.decode("runner reference list")?;
        match list.count {
            1 => list.runner_references.into_iter().next().map_or_else(
                || Err(response.failed(None, "runner reference list count lies about its value")),
                |runner| Ok(Some(runner)),
            ),
            0 => Ok(None),
            _ => Err(ScaleSetError::Local(format!(
                "multiple runners found with name {runner_name:?}"
            ))),
        }
    }

    /// Mirror of `RemoveRunner` (expects 204).
    pub async fn remove_runner(&self, runner_id: i64) -> Result<(), ScaleSetError> {
        let response = self
            .actions_service_request(
                Method::DELETE,
                &format!("/{RUNNER_ENDPOINT}/{runner_id}"),
                &[],
                None,
            )
            .await?;
        if response.status != StatusCode::NO_CONTENT {
            return Err(unexpected_status(&response));
        }
        Ok(())
    }

    /// Mirror of `GenerateJitRunnerConfig`.
    pub async fn generate_jit_runner_config(
        &self,
        setting: &RunnerScaleSetJitRunnerSetting,
        scale_set_id: i32,
    ) -> Result<RunnerScaleSetJitRunnerConfig, ScaleSetError> {
        let body = serde_json::to_vec(setting).map_err(|error| {
            ScaleSetError::Local(format!("failed to marshal runner settings: {error}"))
        })?;
        let response = self
            .actions_service_request(
                Method::POST,
                &format!("/{SCALESET_ENDPOINT}/{scale_set_id}/generatejitconfig"),
                &[],
                Some(body),
            )
            .await?;
        if response.status != StatusCode::OK {
            return Err(unexpected_status(&response));
        }
        response.decode("runner JIT config")
    }

    fn github_api_url(&self, path: &str) -> Result<Url, ScaleSetError> {
        self.inner.config.github_api_url(path).map_err(|error| {
            ScaleSetError::Local(format!("failed to create new GitHub API request: {error}"))
        })
    }

    /// GitHub API request builder (`newGitHubAPIRequest` sets UA only;
    /// callers add auth + content type).
    async fn execute(
        &self,
        method: Method,
        url: Url,
        build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<RawResponse, ScaleSetError> {
        let user_agent = self.user_agent().await;
        self.execute_with_retry(method, url, false, move |client, method, url| {
            build(
                client
                    .request(method, url)
                    .header(USER_AGENT, user_agent.clone()),
            )
        })
        .await
    }

    /// Admin handshake with 401/403 retries (`CheckRetry` override).
    async fn execute_admin_handshake(
        &self,
        url: Url,
        body: Vec<u8>,
        registration_token: &str,
    ) -> Result<RawResponse, ScaleSetError> {
        let user_agent = self.user_agent().await;
        let authorization = format!("RemoteAuth {registration_token}");
        self.execute_with_retry(Method::POST, url, true, move |client, method, url| {
            client
                .request(method, url)
                .header(USER_AGENT, user_agent.clone())
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, authorization.clone())
                .body(body.clone())
        })
        .await
    }

    /// Queue request issued with the session token (used by the session
    /// client for GET poll; mirrors the header set in `getMessage`).
    pub(crate) async fn execute_queue_request(
        &self,
        method: Method,
        url: Url,
        headers: Vec<(String, String)>,
    ) -> Result<RawResponse, ScaleSetError> {
        self.execute_with_retry(method, url, false, move |client, method, url| {
            let mut request = client.request(method, url);
            for (name, value) in &headers {
                request = request.header(name.as_str(), value.as_str());
            }
            request
        })
        .await
    }

    /// Retry loop over `retryablehttp` semantics (see [`RetryPolicy`]).
    async fn execute_with_retry(
        &self,
        method: Method,
        url: Url,
        admin_handshake: bool,
        build: impl Fn(&Client, Method, Url) -> reqwest::RequestBuilder,
    ) -> Result<RawResponse, ScaleSetError> {
        let mut attempt: u32 = 0;
        let use_curl = std::env::var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV).as_deref() == Ok("curl");
        let timeout = self.inner.retry.timeout;
        loop {
            let request = build(&self.inner.http, method.clone(), url.clone())
                .build()
                .map_err(|error| {
                    ScaleSetError::Local(format!(
                        "failed to create new request with context: {error}"
                    ))
                })?;
            let method_name = request.method().to_string();
            let url_text = request.url().to_string();

            if use_curl {
                match tokio::task::spawn_blocking(move || run_curl_raw_request(&request, timeout)).await {
                    Ok(Ok(response)) => {
                        if RetryPolicy::retryable_status(response.status, admin_handshake)
                            && self.inner.retry.may_retry(attempt)
                        {
                            let delay = self.retry_delay(response.status, &response.headers, attempt).await;
                            attempt += 1;
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Ok(response);
                    }
                    Ok(Err(_error)) if self.inner.retry.may_retry(attempt) => {
                        let delay = self.inner.retry.delay_for_attempt(attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(join_err) => {
                        return Err(ScaleSetError::Transport(format!("curl join error: {join_err}")));
                    }
                }
            } else {
                match self.inner.http.execute(request).await {
                    Ok(response) => {
                        let status = response.status();
                        let headers = response.headers().clone();
                        let body = response.bytes().await.map_err(|error| {
                            ScaleSetError::Transport(format!(
                                "failed to read the response body: {error}"
                            ))
                        })?;
                        if RetryPolicy::retryable_status(status, admin_handshake)
                            && self.inner.retry.may_retry(attempt)
                        {
                            let delay = self.retry_delay(status, &headers, attempt).await;
                            attempt += 1;
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Ok(RawResponse {
                            method: method_name,
                            url: url_text,
                            status,
                            headers,
                            body: trim_byte_order_mark(&body).to_vec(),
                        });
                    }
                    // Mirror `DefaultRetryPolicy`: retry transient transport
                    // failures, but not deadlines, malformed requests, redirect
                    // loops, or TLS certificate validation errors.
                    Err(error)
                        if self.inner.retry.may_retry(attempt)
                            && RetryPolicy::retryable_transport_error(&error) =>
                    {
                        let delay = self.inner.retry.delay_for_attempt(attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                    }
                    Err(error) => {
                        return Err(ScaleSetError::Transport(format!(
                            "failed to send request: {error}"
                        )));
                    }
                }
            }
        }
    }

    async fn retry_delay(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        attempt: u32,
    ) -> tokio::time::Duration {
        let backoff = self.inner.retry.delay_for_attempt(attempt);
        RetryPolicy::retry_after_delay(status, headers, SystemTime::now()).unwrap_or(backoff)
    }
}

fn run_curl_raw_request(
    request: &reqwest::Request,
    timeout: std::time::Duration,
) -> Result<RawResponse, ScaleSetError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let temp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4();
    let header_in_path = temp_dir.join(format!("velnor-req-hdr-{id}.tmp"));
    let header_out_path = temp_dir.join(format!("velnor-resp-hdr-{id}.tmp"));

    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&header_in_path)?;
        for (name, val) in request.headers() {
            if let Ok(v) = val.to_str() {
                writeln!(file, "{}: {}", name.as_str(), v)?;
            }
        }
        file.flush()?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&header_in_path);
        return Err(ScaleSetError::Local(format!("failed to write curl header file: {e}")));
    }

    let method_str = request.method().as_str();
    let url_str = request.url().as_str();
    let max_time_secs = timeout.as_secs().max(1);

    let mut cmd = Command::new("curl");
    cmd.arg("--disable")
        .arg("--silent")
        .arg("--show-error")
        .arg("--request")
        .arg(method_str)
        .arg("--url")
        .arg(url_str)
        .arg("-H")
        .arg(format!("@{}", header_in_path.display()))
        .arg("--dump-header")
        .arg(&header_out_path)
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .arg("--retry")
        .arg("0");

    let body_bytes = request.body().and_then(|b| b.as_bytes());
    if body_bytes.is_some() {
        cmd.arg("--data-binary").arg("@-");
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let spawn_res = cmd.spawn();
    let mut child = match spawn_res {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&header_in_path);
            let _ = std::fs::remove_file(&header_out_path);
            return Err(ScaleSetError::Transport(format!("spawn curl: {e}")));
        }
    };

    if let Some(bytes) = body_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(bytes);
        }
    }

    let output_res = child.wait_with_output();
    let _ = std::fs::remove_file(&header_in_path);

    let output = match output_res {
        Ok(output) => output,
        Err(e) => {
            let _ = std::fs::remove_file(&header_out_path);
            return Err(ScaleSetError::Transport(format!("wait curl: {e}")));
        }
    };

    let resp_headers_bytes = std::fs::read(&header_out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&header_out_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ScaleSetError::Transport(format!(
            "curl request {method_str} {url_str} exited with {}: {stderr}",
            output.status
        )));
    }

    let (status, headers) = parse_curl_headers(&resp_headers_bytes)?;

    Ok(RawResponse {
        method: method_str.to_string(),
        url: url_str.to_string(),
        status,
        headers,
        body: trim_byte_order_mark(&output.stdout).to_vec(),
    })
}

fn parse_curl_headers(bytes: &[u8]) -> Result<(StatusCode, HeaderMap), ScaleSetError> {
    let mut status = StatusCode::OK;
    let mut headers = HeaderMap::new();

    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("HTTP/") {
            headers.clear();
            let mut parts = line.split_whitespace();
            parts.next(); // skip HTTP version
            if let Some(code_str) = parts.next() {
                if let Ok(code) = code_str.parse::<u16>() {
                    if let Ok(sc) = StatusCode::from_u16(code) {
                        status = sc;
                    }
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if let (Ok(hname), Ok(hval)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                headers.append(hname, hval);
            }
        }
    }

    Ok((status, headers))
}

/// Mirror of `setUserAgent`: the UA is JSON, `kind` is always `scaleset`.
fn user_agent_string(info: &SystemInfo) -> String {
    #[derive(Debug, Serialize)]
    struct UserAgent<'a> {
        system: &'a str,
        version: &'a str,
        commit_sha: &'a str,
        scale_set_id: i32,
        subsystem: &'a str,
        build_version: &'a str,
        build_commit_sha: &'a str,
        kind: &'a str,
    }
    serde_json::to_string(&UserAgent {
        system: &info.system,
        version: &info.version,
        commit_sha: &info.commit_sha,
        scale_set_id: info.scale_set_id,
        subsystem: &info.subsystem,
        build_version: env!("CARGO_PKG_VERSION"),
        build_commit_sha: option_env!("VELNOR_BUILD_SHA").unwrap_or("unknown"),
        kind: "scaleset",
    })
    .unwrap_or_else(|_| "velnor-scaleset".to_string())
}

/// Mirror of `applyDefaultLabelTypes`: empty label types become `System`.
fn apply_default_label_types(set: &mut RunnerScaleSet) {
    for label in &mut set.labels {
        if label.label_type.is_empty() {
            label.label_type = "System".to_string();
        }
    }
}

/// Mirror of `ensureLabels`: name-derived `System` label when labels are empty.
fn ensure_labels(set: &mut RunnerScaleSet) -> Result<(), ScaleSetError> {
    if !set.labels.is_empty() {
        return Ok(());
    }
    if set.name.is_empty() {
        return Err(ScaleSetError::Local(
            "validating runner scale set: runner scale set must have a name or at least one label"
                .to_string(),
        ));
    }
    set.labels.push(velnor_model::ScaleSetLabel {
        label_type: "System".to_string(),
        name: set.name.clone(),
    });
    Ok(())
}

/// Mirror of `actionsServiceAdminToken.requestURL` + `joinURLPath`:
/// path queries merge with explicit query pairs, `api-version` defaults to
/// `6.0-preview` when absent.
fn actions_request_url(
    base: &str,
    path: &str,
    query: &[(String, String)],
) -> Result<Url, ScaleSetError> {
    let (path_only, path_query) = match path.split_once('?') {
        Some((head, tail)) => (head, Some(tail)),
        None => (path, None),
    };
    let joined = join_url_path(base, path_only);
    let mut url = Url::parse(&joined)
        .map_err(|error| ScaleSetError::Local(format!("failed to parse actions URL: {error}")))?;
    {
        let mut pairs = url.query_pairs_mut();
        if let Some(raw) = path_query {
            for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
                pairs.append_pair(&key, &value);
            }
        }
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let has_version = url.query_pairs().any(|(key, _)| key == "api-version");
    if !has_version {
        url.query_pairs_mut()
            .append_pair("api-version", SCALESET_API_VERSION);
    }
    Ok(url)
}

/// Mirror of `joinURLPath`.
fn join_url_path(base: &str, path: &str) -> String {
    if base.is_empty() {
        if path.is_empty() {
            return String::new();
        }
        if path.starts_with('/') {
            return path.to_string();
        }
        return format!("/{path}");
    }
    if path.is_empty() {
        return base.trim_end_matches('/').to_string();
    }
    if path.starts_with('/') {
        return format!("{}{}", base.trim_end_matches('/'), path);
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

/// Mirror of `actionsServiceAdminTokenExpiresAt`: unverified `exp` claim read.
fn admin_token_expires_at(jwt: &str) -> Result<u64> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let payload = jwt.split('.').nth(1).context("failed to parse jwt token")?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .context("failed to parse jwt token")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&decoded).context("failed to parse jwt token")?;
    claims
        .get("exp")
        .and_then(serde_json::Value::as_u64)
        .context("failed to parse token claims to get expire at")
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")
        .map(|d| d.as_secs())
}

fn unexpected_status(response: &RawResponse) -> ScaleSetError {
    // No explicit sentinel: upstream passes a plain error here so the body
    // exception mapping runs; the status-code fault wraps after (inside
    // `request_response_error`, mirroring `wrapResponseErrorType`).
    response.failed(
        None,
        &format!("unexpected status code: {}", response.status.as_u16()),
    )
}

#[derive(serde::Deserialize)]
struct RegistrationToken {
    #[serde(default)]
    token: String,
}

#[derive(serde::Deserialize)]
struct AdminConnectionWire {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

struct AdminConnection {
    actions_service_url: String,
    admin_token: String,
}

impl std::fmt::Debug for AdminConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminConnection")
            .field(
                "actions_service_url",
                &redacted_authenticated_url(&self.actions_service_url),
            )
            .field("admin_token", &"<redacted>")
            .finish()
    }
}

/// Header value for test doubles: expose the UA for assertion.
#[cfg(test)]
pub(crate) fn test_user_agent(info: &SystemInfo) -> String {
    user_agent_string(info)
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

    fn system_info() -> SystemInfo {
        SystemInfo {
            system: "velnor".into(),
            version: "0.1.0".into(),
            commit_sha: "abc123".into(),
            scale_set_id: 7,
            subsystem: "listener".into(),
        }
    }

    #[test]
    fn user_agent_is_json_with_kind_scaleset() {
        let ua = test_user_agent(&system_info());
        let parsed: serde_json::Value = serde_json::from_str(&ua).unwrap();
        assert_eq!(parsed["system"], "velnor");
        assert_eq!(parsed["scale_set_id"], 7);
        assert_eq!(parsed["subsystem"], "listener");
        assert_eq!(parsed["kind"], "scaleset");
        assert!(parsed["build_version"].is_string());
    }

    #[test]
    fn join_url_path_matches_upstream() {
        assert_eq!(join_url_path("", ""), "");
        assert_eq!(join_url_path("", "a"), "/a");
        assert_eq!(join_url_path("", "/a"), "/a");
        assert_eq!(
            join_url_path("https://h.example/x/", ""),
            "https://h.example/x"
        );
        assert_eq!(
            join_url_path("https://h.example/x/", "/p"),
            "https://h.example/x/p"
        );
        assert_eq!(
            join_url_path("https://h.example/x", "p"),
            "https://h.example/x/p"
        );
    }

    #[test]
    fn actions_url_defaults_api_version_and_merges_queries() {
        let url = actions_request_url(
            "https://actions.example/tenant",
            "_apis/runtime/runnerscalesets",
            &[("runnerGroupId".into(), "1".into())],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://actions.example/tenant/_apis/runtime/runnerscalesets?runnerGroupId=1&api-version=6.0-preview"
        );
        let explicit =
            actions_request_url("https://actions.example/tenant", "p?api-version=5.0", &[])
                .unwrap();
        assert!(explicit.as_str().contains("api-version=5.0"));
        assert_eq!(
            explicit
                .query_pairs()
                .filter(|(k, _)| k == "api-version")
                .count(),
            1
        );
    }

    #[test]
    fn admin_token_expiry_reads_unverified_exp() {
        // {"exp": 2000000000} payload, unsigned segments (signature never read).
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjIwMDAwMDAwMDB9.sig";
        assert_eq!(admin_token_expires_at(jwt).unwrap(), 2_000_000_000);
        assert!(admin_token_expires_at("not-a-jwt").is_err());
        assert!(admin_token_expires_at("a.eyJub2V4cCI6MX0.c").is_err());
    }

    #[test]
    fn admin_token_debug_redacts_authorization_header() {
        let token = AdminToken {
            authorization_header: "Bearer live-admin-token".into(),
            expires_at_epoch: 2_000_000_000,
            url: "https://actions.invalid/tenant?sig=admin-url-secret".into(),
        };
        let rendered = format!("{token:?}");
        assert!(
            !rendered.contains("live-admin-token"),
            "admin token Debug leaked: {rendered}"
        );
        assert!(rendered.contains("https://actions.invalid/tenant"));
        assert!(!rendered.contains("admin-url-secret"), "{rendered}");
    }

    #[test]
    fn ensure_labels_derives_name_label() {
        let mut set = RunnerScaleSet {
            id: 0,
            name: "velnor-set".into(),
            runner_group_id: 0,
            runner_group_name: String::new(),
            labels: vec![],
            runner_setting: velnor_model::RunnerSetting::default(),
            created_on: String::new(),
            runner_jit_config_url: String::new(),
            statistics: None,
        };
        ensure_labels(&mut set).unwrap();
        assert_eq!(set.labels.len(), 1);
        assert_eq!(set.labels[0].name, "velnor-set");
        assert_eq!(set.labels[0].label_type, "System");

        set.name.clear();
        set.labels.clear();
        assert!(ensure_labels(&mut set).is_err());
    }

    #[test]
    fn default_label_types_fill_system() {
        let mut set = RunnerScaleSet {
            id: 0,
            name: String::new(),
            runner_group_id: 0,
            runner_group_name: String::new(),
            labels: vec![velnor_model::ScaleSetLabel {
                label_type: String::new(),
                name: "velnor".into(),
            }],
            runner_setting: velnor_model::RunnerSetting::default(),
            created_on: String::new(),
            runner_jit_config_url: String::new(),
            statistics: None,
        };
        apply_default_label_types(&mut set);
        assert_eq!(set.labels[0].label_type, "System");
    }

    #[test]
    fn pat_constructor_rejects_bad_config_url() {
        assert!(ScaleSetClient::new_with_pat(
            "not-a-url",
            "pat",
            system_info(),
            RetryPolicy::default()
        )
        .is_err());
    }

    #[test]
    fn jwt_provider_constructor_requires_installation_id() {
        let provider: Arc<dyn crate::scaleset::JwtProvider> =
            Arc::new(crate::scaleset::credentials::FnJwtProvider(|| {
                Ok::<_, anyhow::Error>("jwt".to_string())
            }));
        assert!(ScaleSetClient::new_with_jwt_provider(
            "https://github.com/octo-org",
            0,
            provider,
            system_info(),
            RetryPolicy::default()
        )
        .is_err());
    }

    #[test]
    fn admin_token_debug_redacts_the_header() {
        let token = AdminToken {
            authorization_header: "Bearer live-admin-token-bytes".into(),
            expires_at_epoch: 1_000_000,
            url: "https://actions.example/endpoint".into(),
        };
        let rendered = format!("{token:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("live-admin-token-bytes"), "{rendered}");
    }

    #[test]
    fn response_and_connection_debug_redact_protocol_secrets() {
        let response = RawResponse {
            method: "GET".into(),
            url: "https://queue.example/messages?sig=url-secret".into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HeaderMap::from_iter([(
                reqwest::header::SET_COOKIE,
                "session=header-secret".parse().unwrap(),
            )]),
            body: b"body-secret".to_vec(),
        };
        let rendered = format!("{response:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        for secret in ["url-secret", "header-secret", "body-secret"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }

        let connection = AdminConnection {
            actions_service_url: "https://actions.example/tenant?sig=connection-url-secret".into(),
            admin_token: "connection-token-secret".into(),
        };
        let rendered = format!("{connection:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("connection-url-secret"), "{rendered}");
        assert!(!rendered.contains("connection-token-secret"), "{rendered}");
    }

    #[test]
    fn client_debug_redacts_credentials_in_config_url() {
        let client = ScaleSetClient::new(
            "https://github.com/octo-org?sig=config-url-secret",
            ActionsAuth::pat("pat-secret".into()),
            system_info(),
            RetryPolicy::default(),
        )
        .unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("config-url-secret"), "{rendered}");
        assert!(!rendered.contains("pat-secret"), "{rendered}");
    }
}
