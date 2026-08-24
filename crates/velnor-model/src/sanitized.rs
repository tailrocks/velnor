//! Redaction-by-construction DTOs.
//!
//! These are the only types used to carry repository, endpoint, and secret
//! references into serializable resources. Each one stores a projection that
//! cannot represent its secret-bearing source input: token strings,
//! credential-bearing URLs, and secret values are dropped at construction.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An `owner/name` repository reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryRef {
    pub owner: String,
    pub name: String,
}

impl RepositoryRef {
    #[must_use]
    pub fn new(owner: &str, name: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        }
    }

    /// `owner/name`.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

impl fmt::Display for RepositoryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full_name())
    }
}

/// A URL projected to its non-credential parts.
///
/// Construction strips any userinfo component (`user:password@`) so an
/// endpoint URL with embedded credentials can never be serialized.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SanitizedUrl(String);

impl SanitizedUrl {
    /// Project `raw` down to its scheme + host (+ port/path) form.
    ///
    /// Any userinfo segment is removed; if parsing fails entirely the value
    /// degrades to the empty string rather than echoing unvetted input.
    #[must_use]
    pub fn project(raw: &str) -> Self {
        match url::Url::parse(raw) {
            Ok(mut parsed) => {
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
                Self(parsed.to_string())
            }
            Err(_) => Self(String::new()),
        }
    }

    /// The sanitized projection; never contains credentials.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SanitizedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named secret variable: carries only the name, never the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRef {
    pub name: String,
}

impl SecretRef {
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }
}

/// A GitHub App or integration identity projected to display metadata only.
///
/// IDs and slugs are safe; private keys, client secrets, and tokens have no
/// field here at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityRef {
    pub slug: String,
    pub id: Option<u64>,
}

impl IdentityRef {
    #[must_use]
    pub fn new(slug: &str, id: Option<u64>) -> Self {
        Self {
            slug: slug.to_owned(),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_url_strips_userinfo_credentials() {
        let raw = "https://octocat:ghp_supersecret@github.example.com/octo/hook";
        let clean = SanitizedUrl::project(raw);
        assert_eq!(clean.as_str(), "https://github.example.com/octo/hook");
        assert!(!clean.as_str().contains("ghp_"));
        assert!(!clean.as_str().contains("octocat"));
    }

    #[test]
    fn sanitized_url_without_credentials_is_preserved() {
        let clean = SanitizedUrl::project("https://velnor.example.com/status");
        assert_eq!(clean.as_str(), "https://velnor.example.com/status");
    }

    #[test]
    fn sanitized_url_degrades_unparseable_input_to_empty() {
        assert_eq!(SanitizedUrl::project("not a url at all").as_str(), "");
    }

    #[test]
    fn secret_ref_serializes_name_only() {
        let json = serde_json::to_string(&SecretRef::named("DEPLOY_TOKEN")).unwrap();
        assert_eq!(json, "{\"name\":\"DEPLOY_TOKEN\"}");
    }

    #[test]
    fn repository_ref_displays_owner_slash_name() {
        assert_eq!(
            RepositoryRef::new("tailrocks", "velnor-actions-fixture").to_string(),
            "tailrocks/velnor-actions-fixture"
        );
    }
}
