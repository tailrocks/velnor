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
    ValidatedService, ValidatedStep,
};
pub use cache_transport::{publish_on_success, CacheBlob, CacheTransportError};
pub use docker::DockerBackend;
pub use firecracker::{
    create_golden_snapshot, restore_or_cold_boot, FirecrackerApi, FirecrackerBackend,
    RecordingFirecracker, FIRECRACKER_GUEST_CID,
};
pub use guest::{
    required_kconfig_for_arch, validate_built_kernel_config, validate_guest_toml,
    validate_kernel_config, validate_rootfs_packages, KERNEL_TARBALL, KERNEL_TARBALL_SHA256,
    KERNEL_VERSION, ROOTFS_PACKAGES,
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
pub use net::{
    nftables_commands, setup_net_invocations, teardown_is_exact, teardown_net_commands,
    teardown_net_invocations,
};
pub use snapshot::{GuestReady, SnapshotIdentity};
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
    /// Bound how long a single idle `recv` may block. No-op for channels
    /// without an underlying socket timeout (test loopbacks).
    fn set_idle_timeout(&mut self, _timeout: std::time::Duration) {}
}

use std::path::{Path, PathBuf};

use velnor_model::{
    ExecutionBackendKind, ExecutionConfigError, ExecutionFile, MicroVmPreflightFailure,
};

use crate::executor::{CommandResult, CommandRunner, SpawnedProcess};

/// Load `[execution] backend` from `execution.toml`. No env default.
///
/// Search order: `explicit`, `<config_dir>/execution.toml`, then the packaged
/// conffile `/etc/velnor/execution.toml` (the deb ships it there while the
/// controller passes `--state-dir /var/lib/velnor` and daemon slots pass their
/// slot config dirs, so neither of the first two locations holds it on a
/// packaged install). Missing file fails closed naming `[execution] backend`.
///
/// # Errors
/// [`ExecutionConfigError`] when the file is missing or invalid.
pub fn load_execution_file(
    config_dir: &Path,
    explicit: Option<&Path>,
) -> Result<ExecutionFile, ExecutionConfigError> {
    let primary = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config_dir.join(ExecutionFile::FILE_NAME));
    let packaged = Path::new("/etc/velnor").join(ExecutionFile::FILE_NAME);
    load_execution_file_from(&primary, &packaged)
}

fn load_execution_file_from(
    primary: &Path,
    packaged: &Path,
) -> Result<ExecutionFile, ExecutionConfigError> {
    let text = match std::fs::read_to_string(primary) {
        Ok(text) => text,
        Err(_) => std::fs::read_to_string(packaged).map_err(|_| {
            ExecutionConfigError::missing_file(&format!(
                "{} (also tried packaged {})",
                primary.display(),
                packaged.display()
            ))
        })?,
    };
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
    let result = (|| {
        session.reserve(world)?;
        session.prepare(plan, world)?;
        session.start(world)?;
        if plan.cancel_requested {
            session.cancel(world)?;
        } else {
            session.execute(plan, world)?;
        }
        session.collect()
    })();
    let torn = session.teardown(world);
    if file.backend() == ExecutionBackendKind::MicroVm && session.used_host_docker() {
        return Err(ExecutionError::HostDockerForbidden);
    }
    let mut outcome = result?;
    outcome.cleaned = torn?.cleaned;
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
    /// Hex sha256 of a file. The default buffers the whole file; real hosts
    /// stream so multi-hundred-MB rootfs images never sit in RAM.
    fn digest_sha256(&self, path: &Path) -> Result<String, String> {
        let bytes = self.read(path)?;
        Ok(super::execution::artifacts::hex_sha256(&bytes))
    }
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

    fn digest_sha256(&self, path: &Path) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        let mut reader = std::io::BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
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

/// Executor proof. A preflight `executor.ok` file for the docker backend;
/// for MicroVM, one whose recorded generation matches the packaged artifacts.
/// A live host Docker socket is never proof: it is the transitional backend
/// substrate, not evidence that a preflight ran (August 24 class).
#[must_use]
pub fn executor_is_proven(
    state_dir: &Path,
    backend: ExecutionBackendKind,
    _host_docker_socket: &Path,
) -> bool {
    match backend {
        ExecutionBackendKind::Docker => state_dir.join(crate::node::prove::EXECUTOR_OK).is_file(),
        ExecutionBackendKind::MicroVm => executor_is_proven_at(
            state_dir,
            backend,
            _host_docker_socket,
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
    pub next_pid: u32,
    pub fail_spawn: Option<String>,
    pub fail_kill: Option<String>,
    pub spawned: Vec<SpawnedProcess>,
    pub killed: Vec<u32>,
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
            next_pid: 1,
            fail_spawn: None,
            fail_kill: None,
            spawned: Vec::new(),
            killed: Vec::new(),
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

    fn spawn(&mut self, program: &str, args: &[String]) -> anyhow::Result<SpawnedProcess> {
        self.calls.push((program.to_string(), args.to_vec()));
        if let Some(detail) = &self.fail_spawn {
            anyhow::bail!("{program}: {detail}");
        }
        let spawned = SpawnedProcess { pid: self.next_pid };
        self.next_pid = self.next_pid.saturating_add(1);
        self.spawned.push(spawned.clone());
        Ok(spawned)
    }

    fn kill(&mut self, process: &SpawnedProcess) -> anyhow::Result<()> {
        self.calls
            .push(("kill".into(), vec![process.pid.to_string()]));
        if let Some(detail) = &self.fail_kill {
            anyhow::bail!("kill {}: {detail}", process.pid);
        }
        self.killed.push(process.pid);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
