//! Concrete read-only GitHub transport and durable raw response store.
//!
//! The acquisition module owns the request/page state machine.  This module
//! supplies the production boundary it calls: one fixed GitHub API origin,
//! no redirects for authenticated requests, bearer credentials held only in
//! memory, bounded response bodies, and an immutable content-addressed store
//! with a provenance sidecar. GitHub's artifact archive route has one
//! narrowly validated no-auth redirect exception because its documented API
//! contract returns a short-lived signed URL.

use super::{
    AcquisitionFuture, AcquisitionRequest, AcquisitionTransport, AuthIdentity, HttpMethod,
    TransportFailure,
};
use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use std::fmt;
use std::process::Command;
use std::time::Duration;
use url::Url;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const GITHUB_API_VERSION: &str = "2026-03-10";
const USER_AGENT_VALUE: &str = "velnor-tools-g0-live-collector";

/// Concrete authenticated read-only transport.
///
/// `token` never appears in `Debug`, request records, or errors.  The
/// transport rejects any request that is not a GET REST call or a POST to the
/// fixed GraphQL endpoint. Redirects are disabled for authenticated requests;
/// only the exact artifact archive route may perform one validated no-auth
/// follow to GitHub's archive host.
pub struct GithubHttpTransport {
    client: reqwest::Client,
    token: String,
    max_body_bytes: usize,
}

impl fmt::Debug for GithubHttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubHttpTransport")
            .field("max_body_bytes", &self.max_body_bytes)
            .finish_non_exhaustive()
    }
}

impl GithubHttpTransport {
    /// Construct the transport from a token already obtained by the CLI.
    /// Token material is copied into private memory and is never returned.
    pub fn new(token: impl Into<String>) -> Result<Self> {
        Self::with_limits(token, DEFAULT_TIMEOUT, DEFAULT_MAX_BODY_BYTES)
    }

    /// Environment constructor used by the live CLI.  The returned transport
    /// owns the token; callers must not place the returned env value in an
    /// evidence object or log it.
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .context("live GitHub collection requires GITHUB_TOKEN or GH_TOKEN")?;
        Self::new(token)
    }

    /// Resolve the credential through the host's `gh` keyring when no token
    /// environment variable was supplied.  The token is captured only in
    /// this process's private memory; command arguments, diagnostics, and
    /// evidence never contain it.
    pub fn from_env_or_gh() -> Result<Self> {
        if std::env::var_os("GITHUB_TOKEN").is_some() || std::env::var_os("GH_TOKEN").is_some() {
            return Self::from_env();
        }
        let output = Command::new("gh")
            .args(["auth", "token", "--hostname", "github.com"])
            .output()
            .context("resolve GitHub credential through gh keyring")?;
        if !output.status.success() {
            bail!("gh keyring did not return an authenticated GitHub credential");
        }
        let token = String::from_utf8(output.stdout)
            .context("gh keyring returned a non-text GitHub credential")?
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        Self::new(token)
    }

    /// Register the private token with an acquisition identity so response
    /// bytes are masked before crossing the raw store boundary.
    pub fn bind_auth(&self, auth: &mut AuthIdentity) -> Result<()> {
        auth.register_credential(&self.token)
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    #[cfg(test)]
    fn with_limits(
        token: impl Into<String>,
        timeout: Duration,
        max_body_bytes: usize,
    ) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty()
            || token.as_bytes().contains(&0)
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            bail!("live GitHub transport requires a non-empty token");
        }
        if max_body_bytes == 0 {
            bail!("live GitHub transport requires a positive body limit");
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .context("build GitHub read-only HTTP client")?;
        Ok(Self {
            client,
            token,
            max_body_bytes,
        })
    }

    #[cfg(not(test))]
    fn with_limits(
        token: impl Into<String>,
        timeout: Duration,
        max_body_bytes: usize,
    ) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty()
            || token.as_bytes().contains(&0)
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            bail!("live GitHub transport requires a non-empty token");
        }
        if max_body_bytes == 0 {
            bail!("live GitHub transport requires a positive body limit");
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .context("build GitHub read-only HTTP client")?;
        Ok(Self {
            client,
            token,
            max_body_bytes,
        })
    }

    fn request_url(&self, request: &AcquisitionRequest) -> Result<Url, TransportFailure> {
        let endpoint = request
            .api_origin
            .bind(&request.endpoint_or_operation)
            .map_err(|_| TransportFailure::Other)?;
        let mut url = Url::parse(&endpoint).map_err(|_| TransportFailure::Other)?;
        for (key, value) in &request.query {
            url.query_pairs_mut().append_pair(key, value);
        }
        if request.api == super::ApiKind::GraphQl && url.path() != "/graphql" {
            return Err(TransportFailure::Other);
        }
        match (request.api, request.method) {
            (super::ApiKind::Rest, HttpMethod::Get)
            | (super::ApiKind::GraphQl, HttpMethod::Post) => Ok(url),
            _ => Err(TransportFailure::Other),
        }
    }

    fn request_headers(&self) -> Result<HeaderMap, TransportFailure> {
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|_| TransportFailure::Other)?;
        headers.insert(AUTHORIZATION, value);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        Ok(headers)
    }
}

impl AcquisitionTransport for GithubHttpTransport {
    fn send<'a>(
        &'a self,
        request: AcquisitionRequest,
    ) -> AcquisitionFuture<'a, Result<super::TransportResponse, TransportFailure>> {
        Box::pin(async move {
            let url = self.request_url(&request)?;
            let mut builder = match request.method {
                HttpMethod::Get => self.client.get(url.clone()),
                HttpMethod::Post => self.client.post(url.clone()),
            };
            let mut headers = self.request_headers()?;
            if let Some(accept) = request.accept.as_deref() {
                let value = HeaderValue::from_str(accept).map_err(|_| TransportFailure::Other)?;
                headers.insert(ACCEPT, value);
            }
            builder = builder.headers(headers);
            if let Some(body) = request.body {
                if body.len() > self.max_body_bytes {
                    return Err(TransportFailure::Other);
                }
                builder = builder
                    .header("content-type", "application/json")
                    .body(body);
            }
            let response = builder.send().await.map_err(classify_reqwest_error)?;
            let status = response.status().as_u16();
            let (status, headers, body) = if (300..400).contains(&status) {
                // GitHub's artifact archive endpoint intentionally returns a
                // short-lived signed URL.  Only this exact route may follow
                // one redirect, and the bearer credential is never sent to
                // the archive host.  Every other redirect remains rejected.
                if status != 302 || !is_artifact_archive_url(&url) {
                    return Err(TransportFailure::Other);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(TransportFailure::Other)?;
                let redirect_url = Url::parse(location).map_err(|_| TransportFailure::Other)?;
                if !is_allowed_artifact_redirect(&redirect_url) {
                    return Err(TransportFailure::Other);
                }
                let archive_response = self
                    .client
                    .get(redirect_url)
                    .header(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE))
                    .send()
                    .await
                    .map_err(classify_reqwest_error)?;
                let archive_status = archive_response.status().as_u16();
                if (300..400).contains(&archive_status)
                    || archive_response
                        .content_length()
                        .is_some_and(|length| length > self.max_body_bytes as u64)
                {
                    return Err(TransportFailure::Other);
                }
                let archive_headers = safe_response_headers(archive_response.headers());
                let archive_body = archive_response
                    .bytes()
                    .await
                    .map_err(classify_reqwest_error)?;
                if archive_body.len() > self.max_body_bytes {
                    return Err(TransportFailure::Other);
                }
                (archive_status, archive_headers, archive_body)
            } else {
                if response
                    .content_length()
                    .is_some_and(|length| length > self.max_body_bytes as u64)
                {
                    return Err(TransportFailure::Other);
                }
                let headers = safe_response_headers(response.headers());
                let body = response.bytes().await.map_err(classify_reqwest_error)?;
                if body.len() > self.max_body_bytes {
                    return Err(TransportFailure::Other);
                }
                (status, headers, body)
            };
            Ok(super::TransportResponse {
                status,
                headers,
                body: body.to_vec(),
                // Keep provenance bound to the authenticated GitHub API
                // route; the signed redirect URL is credential-bearing and
                // is intentionally not persisted or exposed in diagnostics.
                effective_endpoint: url.to_string(),
            })
        })
    }
}

fn is_artifact_archive_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("api.github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path_segments().is_some_and(|segments| {
            let segments = segments.collect::<Vec<_>>();
            segments.len() == 7
                && segments[0] == "repos"
                && !segments[1].is_empty()
                && !segments[2].is_empty()
                && segments[3] == "actions"
                && segments[4] == "artifacts"
                && segments[5].parse::<u64>().is_ok_and(|id| id > 0)
                && segments[6] == "zip"
        })
}

fn is_allowed_artifact_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("pipelines.actions.githubusercontent.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.path() != "/"
        && url.fragment().is_none()
}

fn classify_reqwest_error(error: reqwest::Error) -> TransportFailure {
    if error.is_timeout() {
        TransportFailure::Timeout
    } else if error.is_connect() {
        TransportFailure::Connection
    } else {
        TransportFailure::Other
    }
}

fn safe_response_headers(headers: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    const ALLOWED: [&str; 11] = [
        "content-type",
        "link",
        "x-github-request-id",
        "x-request-id",
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
        "x-ratelimit-used",
        "x-ratelimit-reset",
        "retry-after",
        "etag",
        "x-oauth-scopes",
    ];
    let mut safe = std::collections::BTreeMap::new();
    for name in ALLOWED {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            safe.insert(name.to_owned(), value.to_owned());
        }
    }
    safe
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "URL literals in transport boundary tests are compile-time-valid fixtures"
)]
mod tests {
    use super::{is_allowed_artifact_redirect, is_artifact_archive_url};
    use url::Url;

    #[test]
    fn only_exact_artifact_route_can_follow_a_redirect() {
        assert!(is_artifact_archive_url(
            &Url::parse("https://api.github.com/repos/acme/project/actions/artifacts/7/zip")
                .unwrap()
        ));
        assert!(!is_artifact_archive_url(
            &Url::parse(
                "https://api.github.com/repos/acme/project/actions/artifacts/7/zip?token=x"
            )
            .unwrap()
        ));
        assert!(!is_artifact_archive_url(
            &Url::parse("https://api.github.com/repos/acme/project/actions/artifacts/7/delete")
                .unwrap()
        ));
    }

    #[test]
    fn archive_redirect_requires_exact_github_pipeline_host_without_userinfo() {
        assert!(is_allowed_artifact_redirect(
            &Url::parse(
                "https://pipelines.actions.githubusercontent.com/signed/archive?token=opaque"
            )
            .unwrap()
        ));
        assert!(!is_allowed_artifact_redirect(
            &Url::parse(
                "https://pipelines.actions.githubusercontent.com.evil/signed/archive?token=opaque"
            )
            .unwrap()
        ));
        assert!(!is_allowed_artifact_redirect(
            &Url::parse(
                "https://user@pipelines.actions.githubusercontent.com/signed/archive?token=opaque"
            )
            .unwrap()
        ));
    }
}
