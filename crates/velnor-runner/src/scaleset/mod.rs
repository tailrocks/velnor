//! Scale-set protocol foundation (D1 part A).
//!
//! Rust port of the `actions/scaleset` wire protocol at
//! [`upstream_pin::UPSTREAM_COMMIT`]: admin-plane client, message-session
//! client (long poll, ACK, AcquireJobs, JIT config), credential chain, and
//! recorded fixtures. The poll→Scale→ACK loop (listener) lands in part B;
//! this module owns the protocol surface only.

pub mod backoff;
pub mod client;
pub mod config;
pub mod credentials;
pub mod errors;
pub mod fixtures;
pub mod session;
pub mod upstream_pin;

pub use backoff::RetryPolicy;
pub use client::{ScaleSetClient, SystemInfo};
pub use config::{GitHubConfig, GitHubScope};
pub use credentials::{ActionsAuth, FnJwtProvider, GitHubAppAuth, JwtProvider, PemJwtProvider};
pub use errors::{RequestFailure, ScaleSetError, ScaleSetFault};
pub use fixtures::{verify_redaction, FixtureManifest, Fixtures, FIXTURE_HOST, REDACTED};
pub use session::{parse_message_response, MessageSessionClient, ParsedMessage};
pub use upstream_pin::{require_pin, UPSTREAM_COMMIT, UPSTREAM_REPO};

use anyhow::Result;

/// Adapter configuration. Credentials live in the client (in-process refresh);
/// job payloads never carry App keys.
#[derive(Debug, Clone)]
pub struct Config {
    /// Enterprise, org, or repository URL, e.g. `https://github.com/octo-org`.
    pub github_config_url: String,
    /// Scale-set ID from `GetRunnerScaleSetByID` or the set name lookup.
    pub scale_set_id: i32,
    /// Session owner name (upstream passes the org/login here).
    pub owner: String,
    /// Auth material: PAT or GitHub App (+ optional custom JWT provider).
    pub auth: ActionsAuth,
    /// Identity block serialized into the `User-Agent` JSON.
    pub system_info: SystemInfo,
    /// Retry policy; defaults mirror upstream (`retryMax=4`, 30s wait cap).
    pub retry: RetryPolicy,
}

/// Build an authenticated admin-plane client from adapter config.
pub fn connect(config: &Config) -> Result<ScaleSetClient> {
    ScaleSetClient::new(
        &config.github_config_url,
        config.auth.clone(),
        config.system_info.clone(),
        config.retry.clone(),
    )
}
