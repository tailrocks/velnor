#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub const RUNNER_VERSION: &str = "2.326.0";
pub const RUNNER_USER_AGENT: &str = "actions-runner/2.326.0 (velnor)";

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
pub struct GitHubAuthResult {
    #[serde(rename = "url")]
    pub server_url: String,
    #[serde(rename = "token_schema")]
    pub token_schema: String,
    #[serde(rename = "token")]
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgent {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "version")]
    pub version: String,
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
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
}

impl TaskAgent {
    pub fn new(name: impl Into<String>, labels: Vec<String>, ephemeral: bool) -> Self {
        Self {
            name: name.into(),
            version: RUNNER_VERSION.to_string(),
            max_parallelism: 1,
            ephemeral,
            disable_update: true,
            labels: labels
                .into_iter()
                .map(|name| AgentLabel { name, r#type: None })
                .collect(),
            authorization: None,
            id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLabel {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentAuthorization {
    #[serde(rename = "authorizationUrl")]
    pub authorization_url: String,
    #[serde(rename = "clientId")]
    pub client_id: String,
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
    pub message_id: i64,
    pub message_type: String,
    pub body: String,
    pub iv_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJobRequestMessage {
    pub request_id: i64,
    pub job_id: String,
    pub job_display_name: String,
    pub message_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResult {
    Succeeded,
    Failed,
    Canceled,
    Skipped,
    Abandoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStatus {
    Online,
    Busy,
    Offline,
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
            false,
        );
        let json = serde_json::to_value(agent).unwrap();

        assert_eq!(json["name"], "velnor-1");
        assert_eq!(json["maxParallelism"], 1);
        assert_eq!(json["labels"][0]["name"], "velnor");
        assert_eq!(json["labels"][1]["name"], "hetzner-sentry-ci");
    }
}
