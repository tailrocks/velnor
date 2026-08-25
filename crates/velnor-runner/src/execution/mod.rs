//! Backend-neutral execution boundary.
//!
//! GitHub-visible semantics stay in the validated plan. Each backend owns
//! sandbox, filesystem, processes, Docker connectivity, net, limits, transport,
//! cancel, and cleanup. There is no fallback from `microvm` to `docker`.

mod artifacts;
mod backend;
mod cache_transport;
mod docker;
mod firecracker;
mod guest;
mod guest_agent;
mod guest_image;
mod guest_runtime;
mod isolation;
mod net;
mod snapshot;
mod unix_api;

pub use artifacts::{
    hex_sha256, packaged_generation, require_coherent_generation, verify_microvm_artifacts,
    ArtifactChecksums, MicroVmArtifactSet, MicroVmGeneration, FIRECRACKER_VERSION, JAILER_VERSION,
    PACKAGED_MICROVM_ROOT,
};
pub use backend::{
    BackendPhase, BackendSession, ExecutionError, ExecutionEvent, ExecutionOutcome, ValidatedPlan,
};
pub use cache_transport::{publish_on_success, CacheBlob, CacheTransportError};
pub use docker::DockerBackend;
pub use firecracker::{
    restore_or_cold_boot, FirecrackerApi, FirecrackerBackend, RecordingFirecracker,
    FIRECRACKER_GUEST_CID,
};
pub use guest::{
    validate_guest_toml, validate_kernel_config, validate_rootfs_packages, KERNEL_TARBALL,
    KERNEL_TARBALL_SHA256, KERNEL_VERSION, ROOTFS_PACKAGES,
};
#[cfg(target_os = "linux")]
pub use guest_agent::{accept_af_vsock, bind_af_vsock};
pub use guest_agent::{serve_guest_session, GuestSessionEnv};
pub use guest_image::{
    build_guest_image, build_guest_image_cli, merged_kernel_fragment, stage_release_dir,
    verify_kernel_tarball, GuestArch, GuestImageRequest, BOOT_KCONFIG,
};
pub use guest_runtime::{
    execute_guest_plan, handle_delivered_plan, host_vsock_connect_path, LoopbackVsock,
    UnixVsockChannel, GUEST_AGENT_PORT,
};
pub use isolation::{IsolationIdentity, IsolationResources};
pub use net::{nftables_commands, teardown_is_exact, teardown_net_commands};
pub use snapshot::SnapshotIdentity;
pub use unix_api::UnixFirecrackerClient;

/// Guest-agent entry: decode a vsock plan and run it on the local Docker daemon.
///
/// # Errors
/// Plan decode or guest Docker failure.
pub fn run_guest_plan_bytes(plan_bytes: &[u8]) -> Result<i32, String> {
    let mut runner = crate::executor::ProcessCommandRunner;
    let mut events = Vec::new();
    handle_delivered_plan(plan_bytes, &mut runner, &mut events)
}

/// Production GitHub job engine owned by the Docker backend.
pub trait ProductionDockerEngine {
    /// # Errors
    /// Docker job execution failures.
    fn execute_github_job(
        &mut self,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError>;
}

/// Host vsock channel (Firecracker UDS or a test loopback).
pub trait VsockChannel {
    /// # Errors
    /// Transport failure.
    fn send(&mut self, message: velnor_model::VsockMessage) -> Result<(), String>;
    /// # Errors
    /// Transport failure.
    fn recv(&mut self) -> Result<velnor_model::VsockMessage, String>;
}

use std::path::{Path, PathBuf};

use velnor_model::{
    ExecutionBackendKind, ExecutionConfigError, ExecutionFile, MicroVmPreflightFailure,
};

use crate::executor::{CommandResult, CommandRunner};

/// Load `[execution] backend` from `execution.toml`. No env default.
///
/// Search order: `explicit`, `<config_dir>/execution.toml`. Missing file fails
/// closed naming `[execution] backend`.
///
/// # Errors
/// [`ExecutionConfigError`] when the file is missing or invalid.
pub fn load_execution_file(
    config_dir: &Path,
    explicit: Option<&Path>,
) -> Result<ExecutionFile, ExecutionConfigError> {
    let path = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config_dir.join(ExecutionFile::FILE_NAME));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| ExecutionConfigError::missing_file(&path.display().to_string()))?;
    ExecutionFile::parse_toml(&text)
}

/// Open a backend session for the selected backend. MicroVM never returns a
/// docker session.
///
/// # Errors
/// Config, preflight, or construction failures.
pub fn open_session(
    file: &ExecutionFile,
    isolation: IsolationIdentity,
    world: &mut ExecutionWorld<'_>,
) -> Result<BackendSession, ExecutionError> {
    match file.backend() {
        ExecutionBackendKind::Docker => BackendSession::docker(isolation, world),
        ExecutionBackendKind::MicroVm => BackendSession::microvm(isolation, world),
    }
}

/// Drive the full lifecycle for one admitted plan. Both backends use this.
///
/// # Errors
/// Preflight, phase, or teardown failures. MicroVM never hops to docker.
pub fn run_validated_job(
    file: &ExecutionFile,
    isolation: IsolationIdentity,
    plan: &ValidatedPlan,
    world: &mut ExecutionWorld<'_>,
) -> Result<ExecutionOutcome, ExecutionError> {
    let mut session = open_session(file, isolation, world)?;
    session.reserve(world)?;
    session.prepare(plan, world)?;
    session.start(world)?;
    let run_result = if plan.cancel_requested {
        session.cancel(world)
    } else {
        session.execute(plan, world)
    };
    let collect_result = if run_result.is_ok() {
        Some(session.collect())
    } else {
        None
    };
    let torn = session.teardown(world);
    if file.backend() == ExecutionBackendKind::MicroVm && session.used_host_docker() {
        return Err(ExecutionError::HostDockerForbidden);
    }
    run_result?;
    let mut outcome = match collect_result {
        Some(result) => result?,
        None => return Err(ExecutionError::CollectBeforeStop),
    };
    let torn = torn?;
    outcome.cleaned = torn.cleaned;
    Ok(outcome)
}

/// Host/guest world used by backends. Tests inject doubles at this boundary.
pub struct ExecutionWorld<'a> {
    pub kvm: &'a Path,
    pub artifact_root: &'a Path,
    pub host_docker_socket: &'a Path,
    pub runner: &'a mut dyn CommandRunner,
    pub firecracker: &'a mut dyn FirecrackerApi,
    pub host_fs: &'a mut dyn HostFs,
    pub vsock: Option<&'a mut dyn VsockChannel>,
    pub docker_engine: Option<&'a mut dyn ProductionDockerEngine>,
    /// Tests may run the guest plan against an injected `CommandRunner`.
    /// Production microVM execute requires [`Self::vsock`].
    pub allow_inline_guest_plan: bool,
}

/// Host filesystem operations used by isolation cleanup and artifact checks.
pub trait HostFs {
    fn exists(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> Result<Vec<u8>, String>;
    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String>;
    fn remove_dir_all(&mut self, path: &Path) -> Result<(), String>;
    fn create_dir_all(&mut self, path: &Path) -> Result<(), String>;
}

/// Real host filesystem.
pub struct RealHostFs;

impl HostFs for RealHostFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Ok(());
        }
        std::fs::remove_dir_all(path).map_err(|error| format!("remove {}: {error}", path.display()))
    }

    fn create_dir_all(&mut self, path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|error| format!("create {}: {error}", path.display()))
    }
}

/// In-memory filesystem for contract tests.
#[derive(Debug, Default)]
pub struct MemoryFs {
    pub files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    pub dirs: std::collections::BTreeSet<PathBuf>,
}

impl HostFs for MemoryFs {
    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.dirs.contains(path)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("missing {}", path.display()))
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            self.dirs.insert(parent.to_path_buf());
        }
        self.files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), String> {
        let prefix = path.to_path_buf();
        self.files.retain(|key, _| !key.starts_with(&prefix));
        self.dirs.retain(|key| !key.starts_with(&prefix));
        Ok(())
    }

    fn create_dir_all(&mut self, path: &Path) -> Result<(), String> {
        self.dirs.insert(path.to_path_buf());
        Ok(())
    }
}

/// Construct a host Docker executor only for the docker backend.
///
/// # Errors
/// [`ExecutionError::HostDockerForbidden`] when `microvm` is selected.
pub fn host_docker_executor<R: CommandRunner>(
    runner: R,
    backend: ExecutionBackendKind,
) -> Result<crate::executor::DockerScriptExecutor<R>, ExecutionError> {
    match backend {
        ExecutionBackendKind::Docker => Ok(crate::executor::DockerScriptExecutor::new(runner)),
        ExecutionBackendKind::MicroVm => Err(ExecutionError::HostDockerForbidden),
    }
}

/// Docker-socket executor proof. MicroVM never treats the host socket as ready.
#[must_use]
pub fn executor_is_proven(
    state_dir: &Path,
    backend: ExecutionBackendKind,
    host_docker_socket: &Path,
) -> bool {
    let ok_file = state_dir.join(crate::node::prove::EXECUTOR_OK);
    match backend {
        ExecutionBackendKind::Docker => ok_file.is_file() || host_docker_socket.exists(),
        ExecutionBackendKind::MicroVm => executor_is_proven_at(
            state_dir,
            backend,
            host_docker_socket,
            Path::new(PACKAGED_MICROVM_ROOT),
        ),
    }
}

/// MicroVM proof: `executor.ok` generation must match live packaged artifacts.
#[must_use]
pub fn executor_is_proven_at(
    state_dir: &Path,
    backend: ExecutionBackendKind,
    host_docker_socket: &Path,
    artifact_root: &Path,
) -> bool {
    match backend {
        ExecutionBackendKind::Docker => executor_is_proven(state_dir, backend, host_docker_socket),
        ExecutionBackendKind::MicroVm => {
            if host_docker_socket_counts(backend) {
                return false;
            }
            let Ok(bytes) = std::fs::read(state_dir.join(crate::node::prove::EXECUTOR_OK)) else {
                return false;
            };
            let Ok(proven) = serde_json::from_slice::<MicroVmGeneration>(&bytes) else {
                return false;
            };
            let fs = RealHostFs;
            let Ok(packaged) = packaged_generation(artifact_root, &fs) else {
                return false;
            };
            require_coherent_generation(&proven, &packaged).is_ok()
        }
    }
}

fn host_docker_socket_counts(backend: ExecutionBackendKind) -> bool {
    backend.uses_host_docker_socket()
}

/// Run backend-specific preflight. MicroVM does not invoke host Docker.
///
/// # Errors
/// Missing requirements fail closed with an exact name.
pub fn preflight_selected(
    file: &ExecutionFile,
    world: &mut ExecutionWorld<'_>,
) -> Result<(), ExecutionError> {
    match file.backend() {
        ExecutionBackendKind::Docker => DockerBackend::preflight(world),
        ExecutionBackendKind::MicroVm => FirecrackerBackend::preflight(world),
    }
}

impl From<ExecutionConfigError> for ExecutionError {
    fn from(error: ExecutionConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<MicroVmPreflightFailure> for ExecutionError {
    fn from(error: MicroVmPreflightFailure) -> Self {
        Self::MicroVm(error)
    }
}

/// Recording command runner for contract tests.
#[derive(Debug)]
pub struct RecordingCommands {
    pub calls: Vec<(String, Vec<String>)>,
    pub next: CommandResult,
    pub codes: Vec<i32>,
}

impl Default for RecordingCommands {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            next: CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            codes: Vec::new(),
        }
    }
}

impl CommandRunner for RecordingCommands {
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<CommandResult> {
        self.calls.push((program.to_string(), args.to_vec()));
        let mut result = self.next.clone();
        if !self.codes.is_empty() {
            result.code = self.codes.remove(0);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
