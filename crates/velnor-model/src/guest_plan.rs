//! Backend-neutral GitHub-visible job plan. Serialized over vsock to the guest.
//!
//! Host signing material, runner registration keys, and org admin tokens are
//! not fields on this type and must never be added.

use serde::{Deserialize, Serialize};

use crate::job_summary::JobConclusion;

/// Serializable plan both backends execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub command_files: Vec<String>,
    pub outputs: Vec<GuestOutput>,
    pub env: Vec<GuestEnvVar>,
    pub workspace: String,
    pub cache: Vec<GuestCacheOp>,
    pub artifacts: Vec<GuestArtifactOp>,
    pub annotations: Vec<String>,
    pub summary: String,
    pub buildx: bool,
    pub testcontainers: bool,
}

/// Job or service environment pair. Never a host docker.sock path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestEnvVar {
    pub name: String,
    pub value: String,
}

/// Service container inside the job (guest Docker or host Docker backend).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct GuestCacheOp {
    pub digest: String,
}

/// Artifact name and guest path for bounded export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestArtifactOp {
    pub name: String,
    pub path: String,
}

/// One workflow step. `script` may be empty when `action` names a native adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestStep {
    pub id: String,
    pub script: String,
    /// Native `uses:` identity when this step is an action.
    #[serde(default)]
    pub action: Option<String>,
    /// Admitted action inputs (clone URL, cache key, paths). Never host sockets.
    #[serde(default)]
    pub inputs: Vec<GuestEnvVar>,
}

/// Declared job output name and admitted value/expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| format!("guest plan decode: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "guest plan decode: expected a JSON object".to_string())?;
        for field in [
            "isolation_id",
            "generation",
            "job_id",
            "image",
            "services",
            "steps",
            "timeout_ms",
            "cancel_requested",
            "fail",
            "cache_digest",
            "command_files",
            "outputs",
            "env",
            "workspace",
            "cache",
            "artifacts",
            "annotations",
            "summary",
            "buildx",
            "testcontainers",
        ] {
            if !object.contains_key(field) {
                return Err(format!("guest plan decode: missing field `{field}`"));
            }
        }
        serde_json::from_value(value).map_err(|error| format!("guest plan decode: {error}"))
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
                inputs: Vec::new(),
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

    #[test]
    fn rejects_unknown_guest_plan_json_keys() {
        let mut value = serde_json::json!({
            "isolation_id": "job-1",
            "generation": 1,
            "job_id": "job-1",
            "image": "velnor/job-ubuntu:26.04",
            "services": [],
            "steps": [],
            "timeout_ms": 1000,
            "cancel_requested": false,
            "fail": false,
            "cache_digest": null,
            "command_files": [],
            "outputs": [],
            "env": [],
            "workspace": "/__w",
            "cache": [],
            "artifacts": [],
            "annotations": [],
            "summary": "",
            "buildx": false,
            "testcontainers": false,
            "unadmitted": "value"
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(GuestJobPlan::decode(&bytes).is_err());

        value.as_object_mut().unwrap().remove("unadmitted");
        assert!(GuestJobPlan::decode(&serde_json::to_vec(&value).unwrap()).is_ok());
    }
}
