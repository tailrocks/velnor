//! GitHub config URL parsing (`config.go` at [`crate::scaleset::UPSTREAM_COMMIT`]).
//!
//! Mirrors `parseGitHubConfigFromURL`, `gitHubAPIURL`, `isHostedGitHubURL`,
//! and `createRegistrationTokenPath` exactly, including the
//! `GITHUB_ACTIONS_FORCE_GHES` escape hatch and the `www.github.com` reroute.

use anyhow::{Context, Result};
use url::Url;

/// Config-URL scope (`gitHubScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubScope {
    Enterprise,
    Organization,
    Repository,
}

/// Parsed config URL (`gitHubConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubConfig {
    /// `config/configURL` — echoed into the admin-connection request body.
    pub config_url: Url,
    pub scope: GitHubScope,
    pub enterprise: String,
    pub organization: String,
    pub repository: String,
    pub is_hosted: bool,
}

impl GitHubConfig {
    /// Mirror of `parseGitHubConfigFromURL`.
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim_matches('/');
        let url = Url::parse(trimmed).with_context(|| format!("failed to parse URL: {input}"))?;
        let is_hosted = is_hosted_github_url(&url);
        let invalid = || {
            anyhow::anyhow!(
                "{url}: invalid config URL, should point to an enterprise, org, or repository"
            )
        };

        let segments: Vec<String> = url
            .path()
            .trim_matches('/')
            .split('/')
            .map(str::to_string)
            .collect();
        match segments.as_slice() {
            [org] if !org.is_empty() => Ok(Self {
                config_url: url,
                scope: GitHubScope::Organization,
                enterprise: String::new(),
                organization: org.clone(),
                repository: String::new(),
                is_hosted,
            }),
            [first, second] => {
                if first.eq_ignore_ascii_case("enterprises") {
                    Ok(Self {
                        config_url: url,
                        scope: GitHubScope::Enterprise,
                        enterprise: second.clone(),
                        organization: String::new(),
                        repository: String::new(),
                        is_hosted,
                    })
                } else {
                    Ok(Self {
                        config_url: url,
                        scope: GitHubScope::Repository,
                        enterprise: String::new(),
                        organization: first.clone(),
                        repository: second.clone(),
                        is_hosted,
                    })
                }
            }
            _ => Err(invalid()),
        }
    }

    /// Mirror of `gitHubAPIURL`: hosted → `api.<host>` (with the
    /// `www.github.com` → `api.github.com` reroute), GHES → `/api/v3`.
    pub fn github_api_url(&self, path: &str) -> Result<Url> {
        let mut result = self.config_url.clone();
        if self.is_hosted {
            let host = self.config_url.host_str().unwrap_or_default();
            if host.eq_ignore_ascii_case("www.github.com") {
                result.set_host(Some("api.github.com"))?;
            } else {
                result.set_host(Some(&format!("api.{host}")))?;
            }
            result.set_path(path);
        } else {
            result.set_path(&format!("/api/v3{path}"));
        }
        Ok(result)
    }

    /// Mirror of `createRegistrationTokenPath`.
    pub fn registration_token_path(&self) -> String {
        match self.scope {
            GitHubScope::Organization => format!(
                "/orgs/{}/actions/runners/registration-token",
                self.organization
            ),
            GitHubScope::Enterprise => format!(
                "/enterprises/{}/actions/runners/registration-token",
                self.enterprise
            ),
            GitHubScope::Repository => format!(
                "/repos/{}/{}/actions/runners/registration-token",
                self.organization, self.repository
            ),
        }
    }
}

/// Mirror of `isHostedGitHubURL`, including the force-GHES override.
fn is_hosted_github_url(url: &Url) -> bool {
    if std::env::var_os("GITHUB_ACTIONS_FORCE_GHES").is_some() {
        return false;
    }
    let host = url.host_str().unwrap_or_default();
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("www.github.com")
        || host.eq_ignore_ascii_case("github.localhost")
        || host.to_ascii_lowercase().ends_with(".ghe.com")
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
    fn org_repo_enterprise_scopes_parse() {
        let org = GitHubConfig::parse("https://github.com/octo-org").unwrap();
        assert_eq!(org.scope, GitHubScope::Organization);
        assert_eq!(org.organization, "octo-org");
        assert!(org.is_hosted);

        let repo = GitHubConfig::parse("https://github.com/octo-org/velnor").unwrap();
        assert_eq!(repo.scope, GitHubScope::Repository);
        assert_eq!(repo.repository, "velnor");

        let ent = GitHubConfig::parse("https://github.com/enterprises/octo-ent").unwrap();
        assert_eq!(ent.scope, GitHubScope::Enterprise);
        assert_eq!(ent.enterprise, "octo-ent");
    }

    #[test]
    fn invalid_urls_rejected() {
        assert!(GitHubConfig::parse("https://github.com/").is_err());
        assert!(GitHubConfig::parse("https://github.com/a/b/c").is_err());
        assert!(GitHubConfig::parse("not-a-url").is_err());
    }

    #[test]
    fn hosted_api_url_uses_api_subdomain() {
        let org = GitHubConfig::parse("https://github.com/octo-org").unwrap();
        assert_eq!(
            org.github_api_url("/orgs/octo-org/actions/runners/registration-token")
                .unwrap()
                .as_str(),
            "https://api.github.com/orgs/octo-org/actions/runners/registration-token"
        );
        let www = GitHubConfig::parse("https://www.github.com/octo-org").unwrap();
        assert!(www
            .github_api_url("/x")
            .unwrap()
            .as_str()
            .starts_with("https://api.github.com/x"));
    }

    #[test]
    fn ghes_api_url_uses_api_v3_prefix() {
        let ghes = GitHubConfig::parse("https://ghe.example.com/octo-org").unwrap();
        assert!(!ghes.is_hosted);
        assert_eq!(
            ghes.github_api_url("/orgs/octo-org/actions/runners/registration-token")
                .unwrap()
                .as_str(),
            "https://ghe.example.com/api/v3/orgs/octo-org/actions/runners/registration-token"
        );
    }

    #[test]
    fn registration_token_paths_match_upstream() {
        let org = GitHubConfig::parse("https://github.com/octo-org").unwrap();
        assert_eq!(
            org.registration_token_path(),
            "/orgs/octo-org/actions/runners/registration-token"
        );
        let repo = GitHubConfig::parse("https://github.com/octo-org/velnor").unwrap();
        assert_eq!(
            repo.registration_token_path(),
            "/repos/octo-org/velnor/actions/runners/registration-token"
        );
        let ent = GitHubConfig::parse("https://github.com/enterprises/octo-ent").unwrap();
        assert_eq!(
            ent.registration_token_path(),
            "/enterprises/octo-ent/actions/runners/registration-token"
        );
    }
}
