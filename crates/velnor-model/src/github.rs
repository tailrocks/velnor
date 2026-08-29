//! GitHub-owned workflow/run DTOs.
//!
//! These types remain separate from local lifecycle resources. GitHub status
//! and conclusions are authoritative; Velnor only adds local enrichment.

use serde::{Deserialize, Serialize};

use crate::sanitized::{RepositoryRef, SanitizedUrl};
use crate::time::Timestamp;

/// GitHub workflow run observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubRun {
    /// Stable repository-scoped run id.
    pub id: u64,
    /// Repository identity.
    pub repository: RepositoryRef,
    /// Human run number.
    pub number: u64,
    /// Attempt number, when supplied by the endpoint.
    pub attempt: u32,
    /// Workflow file path.
    pub workflow: String,
    /// Head SHA.
    pub head_sha: String,
    /// Head branch/ref.
    pub head_branch: String,
    /// Trigger event.
    pub event: String,
    /// Upstream status.
    pub status: String,
    /// Upstream conclusion.
    pub conclusion: Option<String>,
    /// Sanitized browser URL.
    pub url: Option<SanitizedUrl>,
    /// Remote observation time.
    pub observed_at: Timestamp,
}

/// GitHub workflow job observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubJob {
    /// Stable job id.
    pub id: u64,
    /// Owning run id.
    pub run_id: u64,
    /// Attempt number.
    pub attempt: u32,
    /// Job display name.
    pub name: String,
    /// Upstream status.
    pub status: String,
    /// Upstream conclusion.
    pub conclusion: Option<String>,
    /// Runner name when assigned.
    pub runner_name: Option<String>,
    /// Ordered steps.
    pub steps: Vec<GithubStep>,
    /// Remote observation time.
    pub observed_at: Timestamp,
}

/// One GitHub job step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubStep {
    /// Step number.
    pub number: u32,
    /// Display name.
    pub name: String,
    /// Upstream status.
    pub status: String,
    /// Upstream conclusion.
    pub conclusion: Option<String>,
}

/// GitHub artifact metadata; content remains outside durable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubArtifact {
    /// Stable artifact id.
    pub id: u64,
    /// Artifact name.
    pub name: String,
    /// Compressed size.
    pub size_bytes: u64,
    /// Expiry time when supplied.
    pub expires_at: Option<Timestamp>,
    /// Download URL projected without credentials.
    pub archive_url: Option<SanitizedUrl>,
}
