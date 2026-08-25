//! Backend-neutral GitHub-visible job plan. Serialized over vsock to the guest.
//!
//! Host signing material, runner registration keys, and org admin tokens are
//! not fields on this type and must never be added.

use serde::{Deserialize, Serialize};

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
    pub command_files: Vec<String>,
}

/// Service container inside the job (guest Docker or host Docker backend).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestService {
    pub name: String,
    pub image: String,
}

/// One workflow step. `script` may be empty in contract fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestStep {
    pub id: String,
    pub script: String,
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
            }],
            steps: vec![GuestStep {
                id: "run".into(),
                script: "echo hi".into(),
            }],
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into()],
        };
        let bytes = plan.encode().unwrap();
        assert_eq!(GuestJobPlan::decode(&bytes).unwrap(), plan);
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("docker.sock"));
        assert!(!json.contains("signing"));
    }
}
