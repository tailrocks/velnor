//! Operator-selectable execution backends. Exactly `docker` and `microvm`.
//!
//! Selection is per daemon/pool via `[execution] backend` in `execution.toml`.
//! There is no automatic fallback and no silent default between backends.

use serde::{Deserialize, Deserializer, Serialize};

/// Operator-facing execution backend. These are the only accepted values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ExecutionBackendKind {
    /// Host Docker: Velnor uses the host daemon for the job and services.
    #[serde(rename = "docker")]
    Docker,
    /// One jailed Firecracker microVM per job; Docker runs only inside the guest.
    #[serde(rename = "microvm")]
    MicroVm,
}

impl ExecutionBackendKind {
    pub const DOCKER: Self = Self::Docker;
    pub const MICROVM: Self = Self::MicroVm;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::MicroVm => "microvm",
        }
    }

    /// Parse the TOML `backend` value. Unknown values fail closed.
    ///
    /// # Errors
    /// [`ExecutionBackendRejected`] names `[execution] backend` and the value.
    pub fn parse_value(raw: &str) -> Result<Self, ExecutionBackendRejected> {
        match raw.trim() {
            "docker" => Ok(Self::Docker),
            "microvm" => Ok(Self::MicroVm),
            other => Err(ExecutionBackendRejected {
                field: "[execution] backend",
                value: other.to_string(),
            }),
        }
    }

    /// Host Docker socket is part of the docker backend only.
    #[must_use]
    pub fn uses_host_docker_socket(self) -> bool {
        matches!(self, Self::Docker)
    }

    /// Native ARM runner advertising is never implied by an amd64 host.
    #[must_use]
    pub fn advertises_native_arm_on_amd64_host(self) -> bool {
        false
    }
}

impl std::fmt::Display for ExecutionBackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExecutionBackendKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse_value(&raw).map_err(serde::de::Error::custom)
    }
}

/// Root of `/etc/velnor/execution.toml` (and `<config-dir>/execution.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionFile {
    pub execution: ExecutionSection,
}

/// `[execution]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSection {
    pub backend: ExecutionBackendKind,
}

impl ExecutionFile {
    pub const FILE_NAME: &'static str = "execution.toml";

    /// Parse operator TOML. Missing table or unknown backend fails closed.
    ///
    /// # Errors
    /// [`ExecutionConfigError`] names the field.
    pub fn parse_toml(text: &str) -> Result<Self, ExecutionConfigError> {
        let parsed: toml::Value = toml::from_str(text).map_err(|error| ExecutionConfigError {
            field: "[execution] backend",
            detail: format!("execution.toml is not valid TOML: {error}"),
        })?;
        let Some(table) = parsed.get("execution") else {
            return Err(ExecutionConfigError {
                field: "[execution] backend",
                detail: "missing [execution] table".to_string(),
            });
        };
        let Some(backend) = table.get("backend") else {
            return Err(ExecutionConfigError {
                field: "[execution] backend",
                detail: "missing backend".to_string(),
            });
        };
        let Some(raw) = backend.as_str() else {
            return Err(ExecutionConfigError {
                field: "[execution] backend",
                detail: format!("backend must be a string, got {backend}"),
            });
        };
        let backend = ExecutionBackendKind::parse_value(raw).map_err(ExecutionConfigError::from)?;
        Ok(Self {
            execution: ExecutionSection { backend },
        })
    }

    #[must_use]
    pub fn backend(&self) -> ExecutionBackendKind {
        self.execution.backend
    }
}

/// Unknown or missing `[execution] backend`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBackendRejected {
    pub field: &'static str,
    pub value: String,
}

impl std::fmt::Display for ExecutionBackendRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}={:?} is not an accepted execution backend; accepted values are docker, microvm",
            self.field, self.value
        )
    }
}

impl std::error::Error for ExecutionBackendRejected {}

/// Failed to load or parse execution.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionConfigError {
    pub field: &'static str,
    pub detail: String,
}

impl ExecutionConfigError {
    #[must_use]
    pub fn missing_file(path: &str) -> Self {
        Self {
            field: "[execution] backend",
            detail: format!("missing execution.toml at {path}"),
        }
    }
}

impl From<ExecutionBackendRejected> for ExecutionConfigError {
    fn from(error: ExecutionBackendRejected) -> Self {
        Self {
            field: error.field,
            detail: error.to_string(),
        }
    }
}

impl std::fmt::Display for ExecutionConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.detail)
    }
}

impl std::error::Error for ExecutionConfigError {}

/// Why a selected microVM backend cannot run this job (never hop to docker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicroVmPreflightFailure {
    pub requirement: &'static str,
    pub detail: String,
}

impl MicroVmPreflightFailure {
    #[must_use]
    pub fn new(requirement: &'static str, detail: impl Into<String>) -> Self {
        Self {
            requirement,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for MicroVmPreflightFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "execution backend microvm failed closed: missing or unhealthy {} ({}); the docker backend was not used",
            self.requirement, self.detail
        )
    }
}

impl std::error::Error for MicroVmPreflightFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_accepts_only_docker_and_microvm() {
        let docker = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
        assert_eq!(docker.backend(), ExecutionBackendKind::Docker);
        let microvm = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
        assert_eq!(microvm.backend(), ExecutionBackendKind::MicroVm);
        let qemu = ExecutionFile::parse_toml("[execution]\nbackend = \"qemu\"\n").unwrap_err();
        assert_eq!(qemu.field, "[execution] backend");
        assert!(qemu.detail.contains("qemu"), "{qemu}");
        assert!(qemu.detail.contains("docker"), "{qemu}");
        assert!(qemu.detail.contains("microvm"), "{qemu}");
    }

    #[test]
    fn missing_table_or_backend_fails_closed() {
        let missing = ExecutionFile::parse_toml("backend = \"docker\"\n").unwrap_err();
        assert_eq!(missing.field, "[execution] backend");
        assert!(missing.detail.contains("missing [execution]"), "{missing}");
        let no_backend = ExecutionFile::parse_toml("[execution]\n").unwrap_err();
        assert!(
            no_backend.detail.contains("missing backend"),
            "{no_backend}"
        );
    }

    #[test]
    fn json_serializes_operator_names() {
        assert_eq!(
            serde_json::to_string(&ExecutionBackendKind::Docker).unwrap(),
            "\"docker\""
        );
        assert_eq!(
            serde_json::to_string(&ExecutionBackendKind::MicroVm).unwrap(),
            "\"microvm\""
        );
    }

    #[test]
    fn docker_uses_host_socket_microvm_does_not() {
        assert!(ExecutionBackendKind::Docker.uses_host_docker_socket());
        assert!(!ExecutionBackendKind::MicroVm.uses_host_docker_socket());
        assert!(!ExecutionBackendKind::Docker.advertises_native_arm_on_amd64_host());
        assert!(!ExecutionBackendKind::MicroVm.advertises_native_arm_on_amd64_host());
    }

    #[test]
    fn microvm_preflight_failure_never_mentions_fallback() {
        let error = MicroVmPreflightFailure::new("kvm", "/dev/kvm is missing");
        let text = error.to_string();
        assert!(text.contains("failed closed"), "{text}");
        assert!(text.contains("kvm"), "{text}");
        assert!(text.contains("docker backend was not used"), "{text}");
    }
}
