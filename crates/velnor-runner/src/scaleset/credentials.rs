//! GitHub App / PAT credentials (`jwt_provider.go` + `actionsAuth`).
//!
//! App private keys live in the [`JwtProvider`] and never leave the process:
//! jobs receive JIT configs, never keys. Token refresh is in-process
//! ([`ScaleSetClient::update_token_if_needed`][crate::scaleset::ScaleSetClient]
//! mirrors `updateTokenIfNeeded` with the same 60s skew margin).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;

/// GitHub App credentials (`GitHubAppAuth`). All fields required.
#[derive(Debug, Clone)]
pub struct GitHubAppAuth {
    /// Client ID of the application (app ID also works).
    pub client_id: String,
    /// Installation ID of the GitHub App.
    pub installation_id: i64,
    /// App private key in PEM format.
    pub private_key_pem: String,
}

impl GitHubAppAuth {
    /// Mirror of `GitHubAppAuth.Validate`.
    pub fn validate(&self) -> Result<()> {
        if self.client_id.is_empty() {
            anyhow::bail!("client ID is required");
        }
        if self.installation_id == 0 {
            anyhow::bail!("app installation ID is required");
        }
        if self.private_key_pem.is_empty() {
            anyhow::bail!("app private key is required");
        }
        Ok(())
    }
}

/// Short-lived App JWT signer (`JWTProvider`). Implementations must be safe
/// for concurrent use; the JWT is RS256 with `iss`/`iat`/`exp`. The boxed
/// future keeps the trait object-safe for KMS/HSM signers.
pub trait JwtProvider: Send + Sync {
    fn token(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;
}

/// Closure adapter (`JWTProviderFunc`).
#[derive(Clone)]
pub struct FnJwtProvider<F>(pub F);

impl<F> std::fmt::Debug for FnJwtProvider<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnJwtProvider").finish_non_exhaustive()
    }
}

impl<F> JwtProvider for FnJwtProvider<F>
where
    F: Fn() -> Result<String> + Send + Sync,
{
    fn token(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(std::future::ready((self.0)()))
    }
}

/// PEM-key JWT signer (`pemJWTProvider`).
#[derive(Debug, Clone)]
pub struct PemJwtProvider {
    client_id: String,
    private_key_pem: String,
}

impl PemJwtProvider {
    /// Mirror of `newPEMJWTProvider`: parses the key eagerly so bad key
    /// material fails at construction, not at first refresh.
    pub fn new(client_id: &str, private_key_pem: &str) -> Result<Self> {
        EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .context("failed to parse RSA private key from PEM")?;
        Ok(Self {
            client_id: client_id.to_string(),
            private_key_pem: private_key_pem.to_string(),
        })
    }
}

impl JwtProvider for PemJwtProvider {
    fn token(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let key = EncodingKey::from_rsa_pem(self.private_key_pem.as_bytes())
            .context("failed to parse RSA private key from PEM");
        let client_id = self.client_id.clone();
        Box::pin(async move {
            let key = key?;
            sign_app_jwt(&key, &client_id, unix_now()?)
        })
    }
}

/// Mirror of `newGitHubAppJWT`: `iat = now - 60s`, `exp = iat + 9min`,
/// `iss = client_id`, RS256.
fn sign_app_jwt(key: &EncodingKey, client_id: &str, now: u64) -> Result<String> {
    #[derive(Debug, Serialize)]
    struct Claims {
        iss: String,
        iat: u64,
        exp: u64,
    }

    let issued_at = now.saturating_sub(60);
    let claims = Claims {
        iss: client_id.to_string(),
        iat: issued_at,
        exp: issued_at.saturating_add(9 * 60),
    };
    encode(&Header::new(Algorithm::RS256), &claims, key).context("sign app JWT")
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")
        .map(|d| d.as_secs())
}

/// Credential pair (`actionsAuth`): PAT xor (JWT provider + installation ID).
/// `Debug` never prints key material: PAT presence only.
#[derive(Clone)]
pub struct ActionsAuth {
    /// GitHub PAT (mutually exclusive with `jwt_provider`).
    pub token: Option<String>,
    /// JWT signer for GitHub App auth.
    pub jwt_provider: Option<Arc<dyn JwtProvider>>,
    /// App installation ID (used with `jwt_provider`).
    pub installation_id: i64,
}

impl std::fmt::Debug for ActionsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionsAuth")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("jwt_provider", &self.jwt_provider.is_some())
            .field("installation_id", &self.installation_id)
            .finish()
    }
}

impl ActionsAuth {
    #[must_use]
    pub fn pat(token: String) -> Self {
        Self {
            token: Some(token),
            jwt_provider: None,
            installation_id: 0,
        }
    }

    #[must_use]
    pub fn app(provider: Arc<dyn JwtProvider>, installation_id: i64) -> Self {
        Self {
            token: None,
            jwt_provider: Some(provider),
            installation_id,
        }
    }

    /// Mirror of `actionsAuth.validate`.
    pub fn validate(&self) -> Result<()> {
        match (&self.token, &self.jwt_provider) {
            (None, None) => {
                anyhow::bail!("either GitHub App credentials or personal access token is required");
            }
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "cannot provide both GitHub App credentials and personal access token"
                );
            }
            (_, Some(_)) if self.installation_id == 0 => {
                anyhow::bail!("app installation ID is required");
            }
            _ => Ok(()),
        }
    }

    #[must_use]
    pub fn is_pat(&self) -> bool {
        self.token.is_some()
    }
}

/// Installation access-token response (`accessToken`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct InstallationAccessToken {
    pub token: String,
    #[allow(
        dead_code,
        reason = "wire shape mirrors upstream; expiry tracked by admin token"
    )]
    pub expires_at: String,
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

    fn test_key_pem() -> String {
        use rsa::pkcs8::EncodePrivateKey;
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[test]
    fn app_auth_validation_matches_upstream() {
        let valid = GitHubAppAuth {
            client_id: "Iv1.abc".into(),
            installation_id: 123,
            private_key_pem: "pem".into(),
        };
        assert!(valid.validate().is_ok());
        assert!(GitHubAppAuth {
            client_id: String::new(),
            ..valid.clone()
        }
        .validate()
        .is_err());
        assert!(GitHubAppAuth {
            installation_id: 0,
            ..valid.clone()
        }
        .validate()
        .is_err());
        assert!(GitHubAppAuth {
            private_key_pem: String::new(),
            ..valid
        }
        .validate()
        .is_err());
    }

    #[test]
    fn actions_auth_is_pat_xor_app() {
        assert!(ActionsAuth::pat("pat".into()).validate().is_ok());
        assert!(ActionsAuth {
            token: None,
            jwt_provider: None,
            installation_id: 0,
        }
        .validate()
        .is_err());
        let provider: Arc<dyn JwtProvider> =
            Arc::new(PemJwtProvider::new("client", &test_key_pem()).unwrap());
        assert!(ActionsAuth {
            token: Some("pat".into()),
            jwt_provider: Some(Arc::clone(&provider)),
            installation_id: 1,
        }
        .validate()
        .is_err());
        assert!(ActionsAuth {
            token: None,
            jwt_provider: Some(provider),
            installation_id: 0,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn bad_pem_fails_at_construction() {
        assert!(PemJwtProvider::new("client", "not-a-key").is_err());
    }

    #[tokio::test]
    async fn app_jwt_claims_match_upstream_windows() {
        use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
        let pem = test_key_pem();
        let key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
        let jwt = sign_app_jwt(&key, "Iv1.abc", 1_000_000).unwrap();
        let private = rsa::RsaPrivateKey::from_pkcs8_pem(&pem).unwrap();
        let public_pem = private
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let decoding = jsonwebtoken::DecodingKey::from_rsa_pem(public_pem.as_bytes()).unwrap();
        let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        let claims = jsonwebtoken::decode::<serde_json::Value>(&jwt, &decoding, &validation)
            .unwrap()
            .claims;
        assert_eq!(claims["iss"], "Iv1.abc");
        assert_eq!(claims["iat"], 1_000_000 - 60);
        assert_eq!(claims["exp"], 1_000_000 - 60 + 540);
    }

    #[tokio::test]
    async fn fn_provider_adapts_closure() {
        let ok = FnJwtProvider(|| Ok::<_, anyhow::Error>("jwt".to_string()));
        assert_eq!(ok.token().await.unwrap(), "jwt");
    }
}
