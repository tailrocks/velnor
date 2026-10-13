//! Backend-neutral GitHub-visible job plan. Serialized over vsock to the guest.
//!
//! Host signing material, runner registration keys, and org admin tokens are
//! not fields on this type and must never be added.

use serde::{Deserialize, Serialize};

use crate::job_summary::JobConclusion;

/// Compiler-cache implementation selected for a guest job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestCompilerCacheBackend {
    Kache,
    Sccache,
    Off,
}

/// Trust boundary for compiler-cache entries delivered to a guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestCompilerCacheTrustClass {
    Untrusted,
    Trusted,
    Release,
}

/// Explicit compiler-cache transport and namespace contract for one guest.
///
/// The descriptor is plan data, not an RPC implementation. The backend and
/// trust class are typed so guest execution cannot replace them by editing
/// compiler environment variables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestCompilerCacheDescriptor {
    pub backend: GuestCompilerCacheBackend,
    pub trust_class: GuestCompilerCacheTrustClass,
    pub protocol_version: u32,
    pub transport_namespace: String,
}

impl GuestCompilerCacheDescriptor {
    /// Version of this compiler-cache descriptor contract.
    pub const PROTOCOL_VERSION: u32 = 1;
    /// Stable transport identity shared by host and guest plan consumers.
    pub const TRANSPORT_NAMESPACE: &'static str = "velnor/compiler-cache";

    /// Construct an explicit compiler-cache descriptor.
    #[must_use]
    pub fn new(
        backend: GuestCompilerCacheBackend,
        trust_class: GuestCompilerCacheTrustClass,
        transport_namespace: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            trust_class,
            protocol_version: Self::PROTOCOL_VERSION,
            transport_namespace: transport_namespace.into(),
        }
    }

    /// Construct the explicit disabled-cache descriptor used by synthetic
    /// plans that do not have compiler-cache admission data.
    #[must_use]
    pub fn off() -> Self {
        Self::new(
            GuestCompilerCacheBackend::Off,
            GuestCompilerCacheTrustClass::Trusted,
            Self::TRANSPORT_NAMESPACE,
        )
    }

    /// Validate the fixed compiler-cache descriptor contract.
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != Self::PROTOCOL_VERSION {
            return Err(format!(
                "compiler-cache protocol version {} is unsupported; expected {}",
                self.protocol_version,
                Self::PROTOCOL_VERSION
            ));
        }
        if self.transport_namespace != Self::TRANSPORT_NAMESPACE {
            return Err(format!(
                "compiler-cache transport namespace is unsupported; expected `{}`",
                Self::TRANSPORT_NAMESPACE
            ));
        }
        Ok(())
    }
}

/// Serializable plan both backends execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestJobPlan {
    pub isolation_id: String,
    pub generation: u64,
    pub job_id: String,
    /// Owning daemon identity (the slot work directory, matching the executor
    /// path's `velnor.daemon-id` label value) so guest-created Docker objects
    /// join the same ownership reclaim graph.
    pub daemon_id: String,
    pub image: String,
    pub services: Vec<GuestService>,
    pub steps: Vec<GuestStep>,
    pub timeout_ms: u64,
    pub cancel_requested: bool,
    pub fail: bool,
    pub cache_digest: Option<String>,
    pub compiler_cache: GuestCompilerCacheDescriptor,
    pub command_files: Vec<String>,
    pub outputs: Vec<GuestOutput>,
    pub env: Vec<GuestEnvVar>,
    pub workspace: String,
    /// GitHub expression context needed for runtime step resolution. Secrets
    /// are already admitted job inputs and remain inside the isolated guest.
    pub context_data: Vec<(String, serde_json::Value)>,
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
    /// Blob bytes when already in the plan (tests) or after vsock `ImportBlob`.
    #[serde(default)]
    pub bytes: Vec<u8>,
    /// Guest path to materialize or export.
    #[serde(default)]
    pub path: String,
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
    /// Step `env:` pairs. Applied on `docker exec -e`.
    #[serde(default)]
    pub env: Vec<GuestEnvVar>,
    /// Step `working-directory`. Applied on `docker exec -w`.
    #[serde(default)]
    pub working_directory: String,
    /// Runtime `if:` expression. Evaluated against prior guest step state.
    #[serde(default)]
    pub condition: Option<String>,
    /// GitHub `continue-on-error` behavior.
    #[serde(default)]
    pub continue_on_error: bool,
    /// Per-step timeout in milliseconds. `None` uses the runner default.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
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
            "daemon_id",
            "image",
            "services",
            "steps",
            "timeout_ms",
            "cancel_requested",
            "fail",
            "cache_digest",
            "compiler_cache",
            "command_files",
            "outputs",
            "env",
            "workspace",
            "context_data",
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

    /// Validate compiler-cache ownership and compiler-visible environment.
    ///
    /// This is deliberately separate from JSON decoding so callers that build
    /// a plan in memory cannot bypass the same fail-closed boundary.
    pub fn validate_compiler_cache(&self) -> Result<(), String> {
        self.compiler_cache.validate()?;
        self.compiler_cache
            .validate_compiler_cache_environment(&self.env)
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

impl GuestCompilerCacheDescriptor {
    /// Compiler variables owned by the selected descriptor. Every one is
    /// rejected for `Off`; an enabled backend admits only its exact subset.
    pub const RESERVED_ENV_NAMES: [&'static str; 9] = [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "KACHE_CACHE_DIR",
        "KACHE_MAX_SIZE",
        "KACHE_LOCAL_ONLY",
        "KACHE_PREFETCH_ENABLED",
        "SCCACHE_DIR",
        "SCCACHE_CACHE_SIZE",
        "SCCACHE_GHA_ENABLED",
    ];

    /// Validate compiler-cache ownership for arbitrary guest environment
    /// pairs. Workflow, step, and command-file environments all use this
    /// boundary so no alternate wrapper or backend can be introduced later.
    pub fn validate_compiler_cache_environment(&self, env: &[GuestEnvVar]) -> Result<(), String> {
        self.validate_compiler_cache_overrides(env)?;
        for (name, expected_value) in self.expected_compiler_cache_environment() {
            let occurrences = env
                .iter()
                .filter(|variable| variable.name.as_str() == *name)
                .count();
            match occurrences {
                0 => {
                    return Err(format!(
                        "compiler-cache environment `{name}` is missing; expected `{expected_value}`"
                    ));
                }
                1 => {}
                _ => {
                    return Err(format!("compiler-cache environment `{name}` is duplicated"));
                }
            }
        }
        Ok(())
    }

    /// Validate a partial compiler environment override. Empty and ordinary
    /// non-reserved environments are valid; reserved names must belong to the
    /// selected backend, occur once, and carry their exact daemon value.
    pub fn validate_compiler_cache_overrides(&self, env: &[GuestEnvVar]) -> Result<(), String> {
        for variable in env {
            if !Self::RESERVED_ENV_NAMES.contains(&variable.name.as_str()) {
                continue;
            }
            let Some((_, expected_value)) = self
                .expected_compiler_cache_environment()
                .iter()
                .find(|(name, _)| *name == variable.name.as_str())
            else {
                return Err(format!(
                    "compiler-cache environment `{}` conflicts with the descriptor",
                    variable.name
                ));
            };
            let occurrences = env
                .iter()
                .filter(|candidate| candidate.name == variable.name)
                .count();
            if occurrences != 1 {
                return Err(format!(
                    "compiler-cache environment `{}` is duplicated",
                    variable.name
                ));
            }
            if variable.value != *expected_value {
                return Err(format!(
                    "compiler-cache environment `{}` conflicts with the descriptor",
                    variable.name
                ));
            }
        }
        Ok(())
    }

    fn expected_compiler_cache_environment(&self) -> &[(&str, &str)] {
        match self.backend {
            GuestCompilerCacheBackend::Kache => &[
                ("RUSTC_WRAPPER", "kache"),
                ("KACHE_CACHE_DIR", "/var/cache/kache"),
                ("KACHE_MAX_SIZE", "20GiB"),
                ("KACHE_LOCAL_ONLY", "true"),
                ("KACHE_PREFETCH_ENABLED", "false"),
            ],
            GuestCompilerCacheBackend::Sccache => &[
                ("RUSTC_WRAPPER", "sccache"),
                ("SCCACHE_DIR", "/var/cache/sccache"),
                ("SCCACHE_CACHE_SIZE", "20G"),
                ("SCCACHE_GHA_ENABLED", "false"),
            ],
            GuestCompilerCacheBackend::Off => &[],
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
            daemon_id: "test-daemon".into(),
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
                env: Vec::new(),
                working_directory: String::new(),
                condition: None,
                continue_on_error: false,
                timeout_ms: None,
            }],
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            compiler_cache: GuestCompilerCacheDescriptor::new(
                GuestCompilerCacheBackend::Kache,
                GuestCompilerCacheTrustClass::Trusted,
                GuestCompilerCacheDescriptor::TRANSPORT_NAMESPACE,
            ),
            command_files: vec!["GITHUB_OUTPUT".into()],
            outputs: vec![GuestOutput {
                name: "result".into(),
                value: "ok".into(),
            }],
            env: vec![
                GuestEnvVar {
                    name: "CI".into(),
                    value: "true".into(),
                },
                GuestEnvVar {
                    name: "RUSTC_WRAPPER".into(),
                    value: "kache".into(),
                },
                GuestEnvVar {
                    name: "KACHE_CACHE_DIR".into(),
                    value: "/var/cache/kache".into(),
                },
            ],
            workspace: "/__w".into(),
            context_data: Vec::new(),
            cache: vec![GuestCacheOp {
                digest: "abc".into(),
                bytes: Vec::new(),
                path: String::new(),
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
            "daemon_id": "test-daemon",
            "image": "velnor/job-ubuntu:26.04",
            "services": [],
            "steps": [],
            "timeout_ms": 1000,
            "cancel_requested": false,
            "fail": false,
            "cache_digest": null,
            "compiler_cache": {
                "backend": "off",
                "trust_class": "trusted",
                "protocol_version": 1,
                "transport_namespace": "velnor/compiler-cache"
            },
            "command_files": [],
            "outputs": [],
            "env": [],
            "workspace": "/__w",
            "context_data": [],
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

    #[test]
    fn rejects_missing_compiler_cache_descriptor() {
        let mut value = serde_json::json!({
            "isolation_id": "job-1",
            "generation": 1,
            "job_id": "job-1",
            "daemon_id": "test-daemon",
            "image": "velnor/job-ubuntu:26.04",
            "services": [],
            "steps": [],
            "timeout_ms": 1000,
            "cancel_requested": false,
            "fail": false,
            "cache_digest": null,
            "compiler_cache": {
                "backend": "off",
                "trust_class": "trusted",
                "protocol_version": 1,
                "transport_namespace": "velnor/compiler-cache"
            },
            "command_files": [],
            "outputs": [],
            "env": [],
            "workspace": "/__w",
            "context_data": [],
            "cache": [],
            "artifacts": [],
            "annotations": [],
            "summary": "",
            "buildx": false,
            "testcontainers": false
        });
        value.as_object_mut().unwrap().remove("compiler_cache");
        let error = GuestJobPlan::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.contains("missing field `compiler_cache`"), "{error}");
    }

    #[test]
    fn rejects_unknown_compiler_cache_descriptor_keys() {
        let mut value = serde_json::json!({
            "isolation_id": "job-1",
            "generation": 1,
            "job_id": "job-1",
            "daemon_id": "test-daemon",
            "image": "velnor/job-ubuntu:26.04",
            "services": [],
            "steps": [],
            "timeout_ms": 1000,
            "cancel_requested": false,
            "fail": false,
            "cache_digest": null,
            "compiler_cache": {
                "backend": "off",
                "trust_class": "trusted",
                "protocol_version": 1,
                "transport_namespace": "velnor/compiler-cache",
                "unadmitted": true
            },
            "command_files": [],
            "outputs": [],
            "env": [],
            "workspace": "/__w",
            "context_data": [],
            "cache": [],
            "artifacts": [],
            "annotations": [],
            "summary": "",
            "buildx": false,
            "testcontainers": false
        });
        assert!(GuestJobPlan::decode(&serde_json::to_vec(&value).unwrap()).is_err());

        value
            .as_object_mut()
            .unwrap()
            .get_mut("compiler_cache")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("unadmitted");
        assert!(GuestJobPlan::decode(&serde_json::to_vec(&value).unwrap()).is_ok());
    }

    fn minimal_plan() -> GuestJobPlan {
        GuestJobPlan {
            isolation_id: "job-1".into(),
            generation: 1,
            job_id: "job-1".into(),
            daemon_id: "test-daemon".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: Vec::new(),
            steps: Vec::new(),
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            compiler_cache: GuestCompilerCacheDescriptor::off(),
            command_files: Vec::new(),
            outputs: Vec::new(),
            env: Vec::new(),
            workspace: "/__w".into(),
            context_data: Vec::new(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        }
    }

    #[test]
    fn compiler_cache_descriptor_rejects_wrong_protocol_or_namespace() {
        let mut plan = minimal_plan();
        plan.compiler_cache.protocol_version = 2;
        assert!(plan.validate_compiler_cache().is_err());

        plan.compiler_cache.protocol_version = GuestCompilerCacheDescriptor::PROTOCOL_VERSION;
        plan.compiler_cache.transport_namespace = "other/cache".into();
        assert!(plan.validate_compiler_cache().is_err());
    }

    #[test]
    fn compiler_cache_off_rejects_wrapper_and_cache_path_environment() {
        let mut plan = minimal_plan();
        plan.env = vec![GuestEnvVar {
            name: "RUSTC_WRAPPER".into(),
            value: "sccache".into(),
        }];
        assert!(plan.validate_compiler_cache().is_err());

        plan.env = vec![GuestEnvVar {
            name: "SCCACHE_DIR".into(),
            value: "/var/cache/sccache".into(),
        }];
        assert!(plan.validate_compiler_cache().is_err());
    }

    #[test]
    fn compiler_cache_enabled_requires_matching_wrapper_and_path() {
        let mut plan = minimal_plan();
        plan.compiler_cache.backend = GuestCompilerCacheBackend::Kache;
        plan.env = vec![
            GuestEnvVar {
                name: "RUSTC_WRAPPER".into(),
                value: "sccache".into(),
            },
            GuestEnvVar {
                name: "KACHE_CACHE_DIR".into(),
                value: "/var/cache/kache".into(),
            },
            GuestEnvVar {
                name: "KACHE_MAX_SIZE".into(),
                value: "20GiB".into(),
            },
            GuestEnvVar {
                name: "KACHE_LOCAL_ONLY".into(),
                value: "true".into(),
            },
            GuestEnvVar {
                name: "KACHE_PREFETCH_ENABLED".into(),
                value: "false".into(),
            },
        ];
        assert!(plan.validate_compiler_cache().is_err());

        plan.env[0].value = "kache".into();
        plan.env[1].value = "/wrong".into();
        assert!(plan.validate_compiler_cache().is_err());

        plan.env[1].value = "/var/cache/kache".into();
        assert!(plan.validate_compiler_cache().is_ok());
    }

    #[test]
    fn compiler_cache_overrides_allow_empty_ordinary_and_selected_values() {
        let descriptor = GuestCompilerCacheDescriptor::new(
            GuestCompilerCacheBackend::Kache,
            GuestCompilerCacheTrustClass::Trusted,
            GuestCompilerCacheDescriptor::TRANSPORT_NAMESPACE,
        );
        assert!(descriptor.validate_compiler_cache_overrides(&[]).is_ok());
        assert!(descriptor
            .validate_compiler_cache_overrides(&[GuestEnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            }])
            .is_ok());
        assert!(descriptor
            .validate_compiler_cache_overrides(&[GuestEnvVar {
                name: "RUSTC_WRAPPER".into(),
                value: "kache".into(),
            }])
            .is_ok());
    }

    #[test]
    fn compiler_cache_overrides_reject_wrong_backend_values_and_duplicates() {
        let descriptor = GuestCompilerCacheDescriptor::new(
            GuestCompilerCacheBackend::Sccache,
            GuestCompilerCacheTrustClass::Trusted,
            GuestCompilerCacheDescriptor::TRANSPORT_NAMESPACE,
        );
        for env in [
            vec![GuestEnvVar {
                name: "KACHE_MAX_SIZE".into(),
                value: "20GiB".into(),
            }],
            vec![GuestEnvVar {
                name: "SCCACHE_CACHE_SIZE".into(),
                value: "1G".into(),
            }],
            vec![
                GuestEnvVar {
                    name: "SCCACHE_DIR".into(),
                    value: "/var/cache/sccache".into(),
                },
                GuestEnvVar {
                    name: "SCCACHE_DIR".into(),
                    value: "/var/cache/sccache".into(),
                },
            ],
        ] {
            assert!(descriptor.validate_compiler_cache_overrides(&env).is_err());
        }
    }
}
