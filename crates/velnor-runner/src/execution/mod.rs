//! Backend-neutral execution boundary.
//!
//! GitHub-visible semantics stay in the validated plan. Each backend owns
//! sandbox, filesystem, processes, Docker connectivity, net, limits, transport,
//! cancel, and cleanup. There is no fallback from `microvm` to `docker`.

mod artifacts;
mod backend;
mod docker;
mod firecracker;
mod isolation;

pub use artifacts::{
    hex_sha256, verify_microvm_artifacts, ArtifactChecksums, MicroVmArtifactSet,
    FIRECRACKER_VERSION, JAILER_VERSION,
};
pub use backend::{
    BackendPhase, BackendSession, ExecutionError, ExecutionEvent, ExecutionOutcome, ValidatedPlan,
};
pub use docker::DockerBackend;
pub use firecracker::{
    restore_or_cold_boot, FirecrackerApi, FirecrackerBackend, RecordingFirecracker,
};
pub use isolation::{IsolationIdentity, IsolationResources};

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

/// Host/guest world used by backends. Tests inject doubles at this boundary.
pub struct ExecutionWorld<'a> {
    pub kvm: &'a Path,
    pub artifact_root: &'a Path,
    pub host_docker_socket: &'a Path,
    pub runner: &'a mut dyn CommandRunner,
    pub firecracker: &'a mut dyn FirecrackerApi,
    pub host_fs: &'a mut dyn HostFs,
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
        ExecutionBackendKind::MicroVm => ok_file.is_file() && !host_docker_socket_counts(backend),
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
        }
    }
}

impl CommandRunner for RecordingCommands {
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<CommandResult> {
        self.calls.push((program.to_string(), args.to_vec()));
        Ok(self.next.clone())
    }
}

#[cfg(test)]
mod tests;
