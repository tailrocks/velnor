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

    /// Prune, reclaim, doctor, and preflight may talk to the host Docker
    /// socket only when the selected backend is docker. Missing selection is
    /// never treated as docker.
    #[must_use]
    pub fn permits_host_docker_maintenance(backend: Option<Self>) -> bool {
        backend.is_some_and(Self::uses_host_docker_socket)
    }

    /// Why host Docker prune/reclaim must not run. `None` means docker is selected.
    #[must_use]
    pub fn host_docker_maintenance_skip_reason(backend: Option<Self>) -> Option<&'static str> {
        match backend {
            Some(Self::Docker) => None,
            Some(Self::MicroVm) => Some(
                "microvm backend does not use the host Docker socket; docker backend was not used",
            ),
            None => Some("execution backend selection failed; docker backend was not used"),
        }
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
    /// `[acceleration]` table. Absent means maximum; unknown keys fail closed.
    #[serde(default)]
    pub acceleration: AccelerationSection,
}

/// `[execution]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSection {
    pub backend: ExecutionBackendKind,
}

/// `[acceleration]` table: the operator dial for Velnor's performance surface.
///
/// The default of every field is the maximum policy — all acceleration
/// features on — so an absent section (the packaged conffile) is maximum and
/// an operator only ever restricts explicitly. `deny_unknown_fields` keeps a
/// typo from silently ignoring a restriction; runtime env overrides live in
/// the runner's `acceleration` module and always emit degradation records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccelerationSection {
    /// Only `maximum` exists today; any other value fails closed so a typo
    /// can never select a slower, half-implemented mode.
    #[serde(default)]
    pub mode: AccelerationMode,
    #[serde(default)]
    pub target_persistence: TargetPersistenceChoice,
    #[serde(default)]
    pub compiler_cache: CompilerCacheChoice,
    #[serde(default = "default_true")]
    pub typed_stores: bool,
    #[serde(default = "default_true")]
    pub singleflight: bool,
    #[serde(default = "default_true")]
    pub prefetch: bool,
    #[serde(default = "default_true")]
    pub cache_aware_scheduling: bool,
    #[serde(default)]
    pub native_actions: NativeActionsChoice,
    #[serde(default = "default_true")]
    pub buildkit_local: bool,
    #[serde(default)]
    pub result_cache: ResultCacheChoice,
}

impl Default for AccelerationSection {
    fn default() -> Self {
        Self {
            mode: AccelerationMode::Maximum,
            target_persistence: TargetPersistenceChoice::Auto,
            compiler_cache: CompilerCacheChoice::Auto,
            typed_stores: true,
            singleflight: true,
            prefetch: true,
            cache_aware_scheduling: true,
            native_actions: NativeActionsChoice::Prefer,
            buildkit_local: true,
            result_cache: ResultCacheChoice::HermeticOnly,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Acceleration mode. `maximum` is the only mode; the field is the operator
/// contract for future modes and rejects everything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccelerationMode {
    #[serde(rename = "maximum")]
    #[default]
    Maximum,
}

/// `[acceleration] target_persistence`: whether persistent target stores are
/// automatic per job, forced on, or disabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetPersistenceChoice {
    #[serde(rename = "auto")]
    #[default]
    Auto,
    #[serde(rename = "on")]
    On,
    #[serde(rename = "off")]
    Off,
}

/// `[acceleration] compiler_cache`: which local compiler cache engages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompilerCacheChoice {
    #[serde(rename = "auto")]
    #[default]
    Auto,
    #[serde(rename = "kache")]
    Kache,
    #[serde(rename = "sccache")]
    Sccache,
    #[serde(rename = "off")]
    Off,
}

/// `[acceleration] native_actions`. `prefer` is the only policy: approved
/// marketplace actions run as pinned native Rust adapters, never Node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeActionsChoice {
    #[serde(rename = "prefer")]
    #[default]
    Prefer,
}

/// `[acceleration] result_cache`. `hermetic-only` is the only policy: action
/// results are reused only when hermetic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultCacheChoice {
    #[serde(rename = "hermetic-only")]
    #[default]
    HermeticOnly,
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
        let acceleration = match parsed.get("acceleration") {
            None => AccelerationSection::default(),
            Some(table) => table
                .clone()
                .try_into()
                .map_err(|error| ExecutionConfigError {
                    field: "[acceleration]",
                    detail: format!("invalid [acceleration] table: {error}"),
                })?,
        };
        Ok(Self {
            execution: ExecutionSection { backend },
            acceleration,
        })
    }

    #[must_use]
    pub fn backend(&self) -> ExecutionBackendKind {
        self.execution.backend
    }

    /// The `[acceleration]` section; absent sections parse to the maximum
    /// default, so this is never `None`.
    #[must_use]
    pub fn acceleration(&self) -> &AccelerationSection {
        &self.acceleration
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
        assert!(ExecutionBackendKind::permits_host_docker_maintenance(Some(
            ExecutionBackendKind::Docker
        )));
        assert!(!ExecutionBackendKind::permits_host_docker_maintenance(
            Some(ExecutionBackendKind::MicroVm)
        ));
        assert!(!ExecutionBackendKind::permits_host_docker_maintenance(None));
        assert!(
            ExecutionBackendKind::host_docker_maintenance_skip_reason(None)
                .unwrap()
                .contains("execution backend selection failed")
        );
        assert!(
            ExecutionBackendKind::host_docker_maintenance_skip_reason(Some(
                ExecutionBackendKind::MicroVm
            ))
            .unwrap()
            .contains("docker backend was not used")
        );
        assert_eq!(
            ExecutionBackendKind::host_docker_maintenance_skip_reason(Some(
                ExecutionBackendKind::Docker
            )),
            None
        );
    }

    #[test]
    fn acceleration_section_defaults_to_maximum() {
        let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
        assert_eq!(*file.acceleration(), AccelerationSection::default());
        assert_eq!(file.acceleration().mode, AccelerationMode::Maximum);
        assert_eq!(
            file.acceleration().target_persistence,
            TargetPersistenceChoice::Auto
        );
        assert_eq!(
            file.acceleration().compiler_cache,
            CompilerCacheChoice::Auto
        );
        assert!(file.acceleration().typed_stores);
        assert!(file.acceleration().singleflight);
        assert!(file.acceleration().prefetch);
        assert!(file.acceleration().cache_aware_scheduling);
        assert_eq!(
            file.acceleration().native_actions,
            NativeActionsChoice::Prefer
        );
        assert!(file.acceleration().buildkit_local);
        assert_eq!(
            file.acceleration().result_cache,
            ResultCacheChoice::HermeticOnly
        );
    }

    #[test]
    fn acceleration_section_accepts_explicit_values() {
        let file = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\nmode = \"maximum\"\ntarget_persistence = \"off\"\ncompiler_cache = \"kache\"\ntyped_stores = false\nsingleflight = false\nprefetch = false\ncache_aware_scheduling = false\nnative_actions = \"prefer\"\nbuildkit_local = false\nresult_cache = \"hermetic-only\"\n",
        )
        .unwrap();
        let acceleration = file.acceleration();
        assert_eq!(acceleration.mode, AccelerationMode::Maximum);
        assert_eq!(
            acceleration.target_persistence,
            TargetPersistenceChoice::Off
        );
        assert_eq!(acceleration.compiler_cache, CompilerCacheChoice::Kache);
        assert!(!acceleration.typed_stores);
        assert!(!acceleration.singleflight);
        assert!(!acceleration.prefetch);
        assert!(!acceleration.cache_aware_scheduling);
        assert_eq!(acceleration.native_actions, NativeActionsChoice::Prefer);
        assert!(!acceleration.buildkit_local);
        assert_eq!(acceleration.result_cache, ResultCacheChoice::HermeticOnly);
    }

    #[test]
    fn acceleration_section_partial_table_keeps_maximum_defaults() {
        let file = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\ncompiler_cache = \"off\"\n",
        )
        .unwrap();
        assert_eq!(file.acceleration().compiler_cache, CompilerCacheChoice::Off);
        assert!(file.acceleration().prefetch);
    }

    #[test]
    fn acceleration_section_rejects_unknown_keys_and_values() {
        let unknown = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\nturbo = true\n",
        )
        .unwrap_err();
        assert_eq!(unknown.field, "[acceleration]");
        assert!(unknown.detail.contains("turbo"), "{unknown}");

        let bad_mode = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\nmode = \"balanced\"\n",
        )
        .unwrap_err();
        assert_eq!(bad_mode.field, "[acceleration]");

        let bad_backend = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\ncompiler_cache = \"gha\"\n",
        )
        .unwrap_err();
        assert_eq!(bad_backend.field, "[acceleration]");
        assert!(
            bad_backend.detail.contains("compiler_cache"),
            "{bad_backend}"
        );
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
