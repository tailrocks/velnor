//! Backend-neutral GitHub-visible job plan. Serialized over vsock to the guest.
//!
//! Host signing material, runner registration keys, and org admin tokens are
//! not fields on this type and must never be added.

use serde::{Deserialize, Serialize};

use crate::job_summary::JobConclusion;

/// Serializable plan both backends execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestJobPlan {
    pub isolation_id: String,
    pub generation: u64,
    pub job_id: String,
    pub image: String,
    pub services: Vec<GuestService>,
    pub steps: Vec<GuestStep>,
    pub timeout_ms: u64,
    pub cancel_requested: bool,
    pub fail: bool,
    pub cache_digest: Option<String>,
    #[serde(default)]
    pub command_files: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<GuestOutput>,
    #[serde(default)]
    pub env: Vec<GuestEnvVar>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub cache: Vec<GuestCacheOp>,
    #[serde(default)]
    pub artifacts: Vec<GuestArtifactOp>,
    #[serde(default)]
    pub annotations: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub buildx: bool,
    #[serde(default)]
    pub testcontainers: bool,
}

/// Job or service environment pair. Never a host docker.sock path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestEnvVar {
    pub name: String,
    pub value: String,
}

/// Service container inside the job (guest Docker or host Docker backend).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestService {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub network_alias: String,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub env: Vec<GuestEnvVar>,
}

/// Digest-addressed cache import/export. No host bind mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestCacheOp {
    pub digest: String,
}

/// Artifact name and guest path for bounded export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestArtifactOp {
    pub name: String,
    pub path: String,
}

/// One workflow step. `script` may be empty in contract fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestStep {
    pub id: String,
    pub script: String,
    /// Native/JS/Docker `uses:` identity when this step is an action.
    #[serde(default)]
    pub action: Option<String>,
}

/// Declared job output name and admitted value/expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestOutput {
    pub name: String,
    pub value: String,
}

impl GuestJobPlan {
    /// Encode for `VsockMessage::DeliverPlan`.
    ///
    /// # Errors
    /// JSON serialization failure.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| format!("guest plan encode: {error}"))
    }

    /// Decode a vsock plan payload.
    ///
    /// # Errors
    /// JSON or schema failure.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|error| format!("guest plan decode: {error}"))
    }

    /// Isolation label for Docker objects owned by this plan.
    #[must_use]
    pub fn isolation_label(&self) -> String {
        format!("velnor.isolation={}/{}", self.isolation_id, self.generation)
    }

    /// Explicit terminal conclusion from plan flags, before steps run.
    #[must_use]
    pub fn planned_conclusion(&self) -> Option<(JobConclusion, i32)> {
        if self.cancel_requested {
            Some((JobConclusion::Cancelled, 1))
        } else if self.timeout_ms == 0 {
            Some((JobConclusion::TimedOut, 1))
        } else if self.fail {
            Some((JobConclusion::Failure, 1))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejects_host_socket_field() {
        let plan = GuestJobPlan {
            isolation_id: "job-1".into(),
            generation: 1,
            job_id: "job-1".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: vec![GuestService {
                name: "pg".into(),
                image: "postgres:16".into(),
                network_alias: "postgres".into(),
                ports: vec!["5432".into()],
                env: vec![GuestEnvVar {
                    name: "POSTGRES_PASSWORD".into(),
                    value: "ci".into(),
                }],
            }],
            steps: vec![GuestStep {
                id: "run".into(),
                script: "echo hi".into(),
                action: None,
            }],
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into()],
            outputs: vec![GuestOutput {
                name: "result".into(),
                value: "ok".into(),
            }],
            env: vec![GuestEnvVar {
                name: "CI".into(),
                value: "true".into(),
            }],
            workspace: "/__w".into(),
            cache: vec![GuestCacheOp {
                digest: "abc".into(),
            }],
            artifacts: vec![GuestArtifactOp {
                name: "logs".into(),
                path: "/__w/logs".into(),
            }],
            annotations: vec!["notice".into()],
            summary: "ok".into(),
            buildx: true,
            testcontainers: true,
        };
        let bytes = plan.encode().unwrap();
        assert_eq!(GuestJobPlan::decode(&bytes).unwrap(), plan);
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("docker.sock"));
        assert!(!json.contains("signing"));
    }
}
