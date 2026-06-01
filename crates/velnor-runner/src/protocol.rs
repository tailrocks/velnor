#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT},
    Client, Method,
};
use rsa::{
    pkcs8::{EncodePrivateKey, LineEnding},
    rand_core::OsRng,
    traits::PublicKeyParts,
    RsaPrivateKey,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;
use uuid::Uuid;

pub const RUNNER_VERSION: &str = "2.326.0";
pub const RUNNER_USER_AGENT: &str = "actions-runner/2.326.0 (velnor)";
pub const EMPTY_LOCK_TOKEN: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubScope {
    pub original_url: String,
    pub hosted: bool,
    pub api_base_url: Url,
    pub tenant_credential_url: Url,
    pub registration_token_url: Url,
    pub remove_token_url: Url,
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
        let tenant_credential_url = api_base_url.join("actions/runner-registration")?;
        let token_scope = token_scope_path(&segments)?;
        let registration_token_url =
            api_base_url.join(&format!("{token_scope}/actions/runners/registration-token"))?;
        let remove_token_url =
            api_base_url.join(&format!("{token_scope}/actions/runners/remove-token"))?;

        Ok(Self {
            original_url: input.to_string(),
            hosted,
            api_base_url,
            tenant_credential_url,
            registration_token_url,
            remove_token_url,
        })
    }
}

fn is_hosted_github(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com")
}

fn api_base_url(github_url: &Url, hosted: bool) -> Result<Url> {
    let host = github_url.host_str().context("GitHub URL needs host")?;
    if hosted {
        Url::parse(&format!("{}://api.{host}/", github_url.scheme()))
            .context("build GitHub API URL")
    } else {
        Url::parse(&format!("{}://{host}/api/v3/", github_url.scheme()))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantCredentialRequest {
    pub url: String,
    pub runner_event: RunnerEvent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerEvent {
    Register,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRunnerToken {
    #[serde(rename = "token")]
    pub token: String,
    #[serde(rename = "expires_at")]
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubAuthResult {
    #[serde(rename = "url")]
    pub server_url: String,
    #[serde(rename = "token_schema")]
    pub token_schema: String,
    #[serde(rename = "token")]
    pub token: String,
    #[serde(default, rename = "use_v2_flow")]
    pub use_v2_flow: bool,
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

#[derive(Clone)]
pub struct OAuthClient {
    http: Client,
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
    ) -> Result<String> {
        let assertion = build_client_assertion(credentials)?;
        let params = [
            ("grant_type", "client_credentials".to_string()),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".to_string(),
            ),
            ("client_assertion", assertion),
        ];

        let response = self
            .http
            .post(&credentials.authorization_url)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/json")
            .form(&params)
            .send()
            .await
            .context("send OAuth token request")?;

        let status = response.status();
        let text = response.text().await.context("read OAuth token response")?;
        if !status.is_success() && status != reqwest::StatusCode::BAD_REQUEST {
            bail!("OAuth token request failed: status={status}, body={text}");
        }

        let token: OAuthTokenResponse =
            serde_json::from_str(&text).context("parse OAuth token response")?;

        if let Some(error) = token.error {
            bail!(
                "OAuth token request failed: error={error}, description={}",
                token.error_description.unwrap_or_default()
            );
        }

        token
            .access_token
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("OAuth token response missing access_token"))
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
        nbf: now,
        exp: now + 600,
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

impl RegistrationClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent(RUNNER_USER_AGENT)
            .build()
            .context("build GitHub registration HTTP client")?;
        Ok(Self { http })
    }

    pub async fn exchange_tenant_credential(
        &self,
        scope: &GitHubScope,
        runner_token: &str,
        runner_event: RunnerEvent,
    ) -> Result<GitHubAuthResult> {
        let request = TenantCredentialRequest {
            url: scope.original_url.clone(),
            runner_event,
        };

        let response = self
            .http
            .post(scope.tenant_credential_url.clone())
            .header(AUTHORIZATION, format!("RemoteAuth {runner_token}"))
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github.v3+json")
            .json(&request)
            .send()
            .await
            .context("send tenant credential request")?;

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-github-request-id")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "tenant credential request failed: status={status}, request_id={}, body={}",
                request_id.unwrap_or_else(|| "unknown".to_string()),
                body
            );
        }

        response
            .json::<GitHubAuthResult>()
            .await
            .context("parse tenant credential response")
    }

    pub async fn create_runner_registration_token(
        &self,
        scope: &GitHubScope,
        pat: &str,
    ) -> Result<GitHubRunnerToken> {
        self.create_runner_token(scope.registration_token_url.clone(), pat, "registration")
            .await
    }

    pub async fn create_runner_remove_token(
        &self,
        scope: &GitHubScope,
        pat: &str,
    ) -> Result<GitHubRunnerToken> {
        self.create_runner_token(scope.remove_token_url.clone(), pat, "remove")
            .await
    }

    async fn create_runner_token(
        &self,
        url: Url,
        pat: &str,
        token_type: &str,
    ) -> Result<GitHubRunnerToken> {
        let basic = STANDARD.encode(format!("github:{pat}"));
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, format!("basic {basic}"))
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .header(ACCEPT, "application/vnd.github.v3+json")
            .send()
            .await
            .with_context(|| format!("send {token_type} token request"))?;

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-github-request-id")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "runner {token_type} token request failed: status={status}, request_id={}, body={}",
                request_id.unwrap_or_else(|| "unknown".to_string()),
                body
            );
        }

        response
            .json::<GitHubRunnerToken>()
            .await
            .with_context(|| format!("parse {token_type} token response"))
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

    pub async fn create_session(&self, session: &TaskAgentSession) -> Result<TaskAgentSession> {
        let url = broker_session_url(&self.base_url)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(session)
            .send()
            .await
            .context("send create broker session request")?;

        parse_json_response(response, "create broker session").await
    }

    pub async fn delete_session(&self) -> Result<()> {
        let url = broker_session_url(&self.base_url)?;
        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send delete broker session request")?;

        parse_empty_response(response, "delete broker session").await
    }

    pub async fn get_runner_message(
        &self,
        session_id: &str,
        status: RunnerStatus,
        disable_update: bool,
    ) -> Result<Option<TaskAgentMessage>> {
        let url = broker_message_url(&self.base_url, session_id, status, disable_update)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .send()
            .await
            .context("send get broker message request")?;

        parse_optional_json_response(response, "get broker message").await
    }

    pub async fn acknowledge_runner_request(
        &self,
        session_id: &str,
        runner_request_id: &str,
        status: RunnerStatus,
    ) -> Result<()> {
        let url = broker_acknowledge_url(&self.base_url, session_id, status)?;
        let body = json!({ "runnerRequestId": runner_request_id });
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("send broker acknowledge request")?;

        parse_empty_response(response, "acknowledge broker runner request").await
    }
}

#[derive(Clone)]
pub struct RunServiceClient {
    http: Client,
    bearer_token: String,
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
        })
    }

    pub async fn acquire_job(
        &self,
        run_service_url: &str,
        job_message_id: &str,
        runner_os: &str,
        billing_owner_id: Option<&str>,
    ) -> Result<Value> {
        let url = run_service_acquire_job_url(run_service_url)?;
        let body = AcquireJobRequest {
            job_message_id,
            runner_os,
            billing_owner_id,
        };
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("send run-service acquire job request")?;

        parse_json_response(response, "acquire run-service job").await
    }

    pub async fn renew_job(
        &self,
        run_service_url: &str,
        plan_id: &str,
        job_id: &str,
    ) -> Result<RenewJobResponse> {
        let url = run_service_renew_job_url(run_service_url)?;
        let body = RenewJobRequest { plan_id, job_id };
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("send run-service renew job request")?;

        parse_json_response(response, "renew run-service job").await
    }

    pub async fn complete_job(
        &self,
        run_service_url: &str,
        completion: RunServiceCompleteJob,
    ) -> Result<()> {
        let url = run_service_complete_job_url(run_service_url)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header(USER_AGENT, RUNNER_USER_AGENT)
            .json(&completion)
            .send()
            .await
            .context("send run-service complete job request")?;

        parse_empty_response(response, "complete run-service job").await
    }
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

        parse_optional_json_response(response, "get message").await
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
        bail!(
            "{action} failed: status={status}, request_id={}, body={}",
            request_id.unwrap_or_else(|| "unknown".to_string()),
            body
        );
    }

    response
        .json::<T>()
        .await
        .with_context(|| format!("parse {action} response"))
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
        bail!("{action} failed: status={status}, body={text}");
    }

    if text.trim().is_empty() {
        return Ok(None);
    }

    serde_json::from_str::<T>(&text)
        .map(Some)
        .with_context(|| format!("parse {action} response"))
}

async fn parse_empty_response(response: reqwest::Response, action: &str) -> Result<()> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("{action} failed: status={status}, body={body}");
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
    #[serde(rename = "agent")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "osDescription")]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LabelType {
    System,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentMessage {
    #[serde(rename = "messageId")]
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
    #[serde(rename = "billingOwnerId", skip_serializing_if = "Option::is_none")]
    pub billing_owner_id: Option<String>,
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
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "status")]
    pub status: TimelineRecordState,
    #[serde(rename = "conclusion")]
    pub conclusion: TaskResult,
    #[serde(rename = "completed_log_lines")]
    pub completed_log_lines: i64,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_repo_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/donbeave/velnor").unwrap();

        assert!(scope.hosted);
        assert_eq!(scope.api_base_url.as_str(), "https://api.github.com/");
        assert_eq!(
            scope.tenant_credential_url.as_str(),
            "https://api.github.com/actions/runner-registration"
        );
        assert_eq!(
            scope.registration_token_url.as_str(),
            "https://api.github.com/repos/donbeave/velnor/actions/runners/registration-token"
        );
        assert_eq!(
            scope.remove_token_url.as_str(),
            "https://api.github.com/repos/donbeave/velnor/actions/runners/remove-token"
        );
    }

    #[test]
    fn hosted_org_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/ChainArgos").unwrap();

        assert_eq!(
            scope.registration_token_url.as_str(),
            "https://api.github.com/orgs/ChainArgos/actions/runners/registration-token"
        );
    }

    #[test]
    fn hosted_enterprise_scope_builds_expected_urls() {
        let scope = GitHubScope::parse("https://github.com/enterprises/acme").unwrap();

        assert_eq!(
            scope.registration_token_url.as_str(),
            "https://api.github.com/enterprises/acme/actions/runners/registration-token"
        );
    }

    #[test]
    fn enterprise_server_scope_uses_api_v3() {
        let scope = GitHubScope::parse("https://github.example.com/org/repo").unwrap();

        assert!(!scope.hosted);
        assert_eq!(
            scope.registration_token_url.as_str(),
            "https://github.example.com/api/v3/repos/org/repo/actions/runners/registration-token"
        );
        assert_eq!(
            scope.tenant_credential_url.as_str(),
            "https://github.example.com/api/v3/actions/runner-registration"
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
    fn auth_result_accepts_v2_flag() {
        let auth: GitHubAuthResult = serde_json::from_str(
            r#"{
                "url": "https://pipelines.actions.githubusercontent.com/",
                "token_schema": "OAuthAccessToken",
                "token": "secret",
                "use_v2_flow": true
            }"#,
        )
        .unwrap();

        assert_eq!(auth.token_schema, "OAuthAccessToken");
        assert!(auth.use_v2_flow);
    }

    #[test]
    fn runner_token_response_parses_expiry() {
        let token: GitHubRunnerToken = serde_json::from_str(
            r#"{
                "token": "abc",
                "expires_at": "2026-05-31T20:00:00Z"
            }"#,
        )
        .unwrap();

        assert_eq!(token.token, "abc");
        assert_eq!(token.expires_at, "2026-05-31T20:00:00Z");
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
        assert!(query.contains("runnerVersion=2.326.0"));
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
                name: "step".into(),
                status: TimelineRecordState::Completed,
                conclusion: TaskResult::Succeeded,
                completed_log_lines: 2,
            }],
            billing_owner_id: Some("42".into()),
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
                    "completed_log_lines": 2
                }],
                "billingOwnerId": "42"
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
}
