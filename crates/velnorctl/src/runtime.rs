//! Typed clap argument structs for operator commands that still execute
//! through the interim `velnor-runner` facade (Plan 064 / 079).
//!
//! These types are the clap parse source. Conversion into
//! [`velnor_runner::args`] is exhaustive `From`, never a second argv walk.
//! `daemon` is the service entrypoint; `run` is the workflow-run namespace.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::{Args, Subcommand};
use velnor_runner::args as rt;

pub use velnor_runner::scaffold::{dispatch, enforce_admission, init_telemetry, telemetry_dir};

/// Service daemon arguments. Runtime ownership moves to the lifecycle engine
/// during Plan 079; this typed boundary keeps one parser and one conversion.
#[derive(Debug, Clone, Args)]
pub struct DaemonArgs {
    /// Injectable durable operational store path for packaged and test daemons.
    #[arg(long, env = "VELNOR_STATE_DB")]
    pub state_db: Option<PathBuf>,
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,
    #[arg(long)]
    pub target_mvp_labels: bool,
    #[arg(long)]
    pub target_mvp_arm_label: bool,
    #[arg(long)]
    pub replace: bool,
    #[arg(long)]
    pub pool_id: Option<i64>,
    #[arg(long, env = "VELNOR_POOL_NAME")]
    pub pool_name: Option<String>,
    #[arg(long)]
    pub routing_policy_file: Option<PathBuf>,
    #[arg(long)]
    pub dry_run_registration: bool,
    #[arg(long, default_value_t = 1)]
    pub slots: usize,
    #[arg(long)]
    pub max_idle_slot_age_seconds: Option<u64>,
    #[arg(long)]
    pub once: bool,
    #[arg(long)]
    pub idle_timeout_seconds: Option<u64>,
    #[arg(long)]
    pub complete_noop: bool,
    #[arg(long)]
    pub execute_scripts: bool,
    #[arg(long)]
    pub dry_run_jobs: bool,
    #[arg(long)]
    pub dump_job_message: Option<PathBuf>,
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,
    /// Docker `--cpus` limit appended to every job container. Empty leaves the
    /// per-job cap to the derived host budget, which divides the machine
    /// between the provisioned slots; a value here is an operator cap that
    /// only ever narrows that share. Must match `velnor_runner::service`.
    #[arg(long, env = "VELNOR_JOB_CPUS", default_value = "")]
    pub job_cpus: String,
    /// Docker `--memory` limit appended to every job container. Empty leaves
    /// the job uncapped by the daemon; the derived budget still sizes the
    /// compile scheduler. Must match `velnor_runner::service`.
    #[arg(long, env = "VELNOR_JOB_MEMORY", default_value = "")]
    pub job_memory: String,
    /// Pool trust boundary. Flattened from the single declaration in
    /// `velnor_runner::trust_scope`, so this binary and `velnor-runner` cannot
    /// disagree about a security gate.
    #[command(flatten)]
    pub trust: velnor_runner::trust_scope::TrustScopeArg,
    #[arg(
        long,
        env = "VELNOR_EMERGENCY_RESERVE_BYTES",
        default_value_t = 10_737_418_240u64
    )]
    pub emergency_reserve_bytes: u64,
    #[arg(long, env = "VELNOR_JOB_PEAK_BYTES", default_value_t = 32_212_254_720u64)]
    pub job_peak_bytes: u64,
    /// Empty keeps each JavaScript action on its own declared Node runtime
    /// image. Must match `velnor_runner::service`.
    #[arg(long, default_value = "")]
    pub node_action_image: String,
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,
    #[arg(long)]
    pub skip_preflight: bool,
    #[arg(long)]
    pub require_docker_socket: bool,
}

/// Run the daemon with its local API owned by the same application lifetime.
///
/// The API binds before the legacy execution loop starts. All three futures
/// are selected together, and each listener owns its exact socket path for
/// cleanup on normal exit, cancellation, and partial startup failure.
pub async fn run_daemon(args: DaemonArgs) -> anyhow::Result<()> {
    enforce_admission()?;
    crate::http::validate_socket_groups()?;
    let instance = args.name.as_deref().unwrap_or("default");
    let endpoint = velnor_client::UnixEndpoint::from_instance(instance)?;
    let control_path = endpoint.socket_path(velnor_client::SocketKind::Control);
    let admin_path = endpoint.socket_path(velnor_client::SocketKind::Admin);
    crate::http::prepare_instance_dir(
        control_path
            .parent()
            .expect("canonical control socket always has an instance directory"),
    )?;
    let _instance_lock = InstanceLock::acquire(&control_path)?;
    crate::http::remove_stale_socket(&control_path)?;
    crate::http::remove_stale_socket(&admin_path)?;
    let state_path = resolve_state_db_path(args.state_db.as_deref());
    // Carry the resolved path in the typed daemon context. This keeps daemon
    // initialization deterministic without mutating process-global state.
    let mut legacy_args: rt::DaemonArgs = args.clone().into();
    legacy_args.state_db = Some(state_path.clone());
    let command = rt::Command::Daemon(Box::new(legacy_args));
    init_telemetry(telemetry_dir(&command).as_deref());
    let store = Arc::new(velnor_control::store::Store::open(state_path)?);
    let services =
        velnor_control::application::ApplicationServices::with_store(Arc::clone(&store), instance)?;
    let api_state = crate::http::ApiState::from_services_for_instance(&services, instance);
    let control_listener = OwnedUnixListener::new(
        crate::http::bind_unix(
            &control_path,
            crate::http::CONTROL_SOCKET_MODE,
            crate::http::CONTROL_GROUP,
        )?,
        control_path,
    )?;
    let admin_listener = OwnedUnixListener::new(
        crate::http::bind_unix(
            &admin_path,
            crate::http::ADMIN_SOCKET_MODE,
            crate::http::ADMIN_GROUP,
        )?,
        admin_path,
    )?;

    let mut daemon = Box::pin(dispatch(command));
    let (shutdown, _) = tokio::sync::watch::channel(false);
    let mut control_shutdown = shutdown.subscribe();
    let mut admin_shutdown = shutdown.subscribe();
    let mut control_server = Box::pin(control_listener.serve(
        crate::http::control_router(api_state.clone()),
        async move {
            let _ = control_shutdown.changed().await;
        },
    ));
    let mut admin_server = Box::pin(admin_listener.serve(
        crate::http::admin_router(api_state),
        async move {
            let _ = admin_shutdown.changed().await;
        },
    ));
    let result = tokio::select! {
        result = &mut daemon => result,
        result = &mut control_server => result.map_err(anyhow::Error::from).context("control socket server stopped"),
        result = &mut admin_server => result.map_err(anyhow::Error::from).context("admin socket server stopped"),
    };
    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let _ = (&mut control_server).await;
        let _ = (&mut admin_server).await;
    })
    .await;
    result
}

fn resolve_state_db_path(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(velnor_control::store::DEFAULT_STATE_DB_PATH))
}

/// Owns a bound Unix listener and the pathname that must disappear with it.
///
/// Keeping the listener and pathname together closes the partial-startup gap:
/// dropping this value closes the listener before removing its socket path.
struct OwnedUnixListener {
    listener: Option<tokio::net::UnixListener>,
    path: PathBuf,
    identity: SocketIdentity,
}

/// Advisory singleton for one daemon instance. The lock is held for the
/// complete listener lifetime, so compliant siblings cannot race pathname
/// cleanup or bind a second control plane for the same instance.
struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn acquire(socket_path: &Path) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let parent = socket_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket must have a parent for the instance lock",
            )
        })?;
        crate::http::prepare_instance_dir(parent)?;
        let lock_path = parent.join(".daemon.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o660)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let owner = unsafe { libc::geteuid() } as u32;
        if !metadata.is_file()
            || (metadata.uid() != 0 && metadata.uid() != owner)
            || metadata.mode() & 0o002 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance lock is not a trusted regular file",
            ));
        }
        // SAFETY: the descriptor remains open in `InstanceLock` for the
        // entire daemon lifetime; libc only reads the descriptor and flags.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { _file: file })
    }
}

impl OwnedUnixListener {
    fn new(listener: tokio::net::UnixListener, path: PathBuf) -> std::io::Result<Self> {
        Ok(Self {
            identity: SocketIdentity::from_listener(&listener)?,
            listener: Some(listener),
            path,
        })
    }

    async fn serve(
        mut self,
        router: axum::Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), std::io::Error> {
        let listener = self
            .listener
            .take()
            .expect("owned Unix listener must be present before serving");
        crate::http::serve_unix(listener, router, shutdown).await
    }
}

#[derive(Debug, Clone, Copy)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn from_listener(listener: &tokio::net::UnixListener) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;

        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `stat` is writable storage and the descriptor is borrowed
        // from the live listener.
        if unsafe { libc::fstat(listener.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstat initialized the structure on success.
        let stat = unsafe { stat.assume_init() };
        // `st_dev` is `i32` on macOS and `u64` on Linux: the widening cast
        // is load-bearing off Linux, so the same-type cast lint does not
        // apply cross-platform.
        #[allow(clippy::unnecessary_cast)]
        Ok(Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
        })
    }
}

impl Drop for OwnedUnixListener {
    fn drop(&mut self) {
        // The listener is dropped before this destructor runs when `serve`
        // owns it in a local, and directly by field drop otherwise.
        self.listener.take();
        cleanup_socket(&self.path, self.identity);
    }
}

fn cleanup_socket(path: &Path, expected: SocketIdentity) {
    let _ = crate::http::remove_socket_if_unchanged(path, expected.device, expected.inode);
}

impl From<DaemonArgs> for velnor_runner::args::DaemonArgs {
    fn from(args: DaemonArgs) -> Self {
        Self {
            state_db: args.state_db,
            config_dir: args.config_dir,
            url: args.url,
            pat: args.pat,
            name: args.name,
            labels: args.labels,
            target_mvp_labels: args.target_mvp_labels,
            target_mvp_arm_label: args.target_mvp_arm_label,
            replace: args.replace,
            pool_id: args.pool_id,
            pool_name: args.pool_name,
            pool_id_pre_resolved: false,
            routing_policy_file: args.routing_policy_file,
            dry_run_registration: args.dry_run_registration,
            slots: args.slots,
            max_idle_slot_age_seconds: args.max_idle_slot_age_seconds,
            once: args.once,
            idle_timeout_seconds: args.idle_timeout_seconds,
            complete_noop: args.complete_noop,
            execute_scripts: args.execute_scripts,
            dry_run_jobs: args.dry_run_jobs,
            dump_job_message: args.dump_job_message,
            docker_image: args.docker_image,
            job_cpus: args.job_cpus,
            job_memory: args.job_memory,
            trust_scope: args.trust.resolve().into_string(),
            emergency_reserve_bytes: args.emergency_reserve_bytes,
            job_peak_bytes: args.job_peak_bytes,
            node_action_image: args.node_action_image,
            work_dir: args.work_dir,
            docker_host_work_dir: args.docker_host_work_dir,
            skip_preflight: args.skip_preflight,
            require_docker_socket: args.require_docker_socket,
        }
    }
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Host work directory that contains daemon-shared stores. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Store configuration under this directory when deriving the default work dir.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    #[arg(
        long,
        env = "VELNOR_BUDGET_TARGETS_BYTES",
        default_value_t = 214_748_364_800u64
    )]
    pub budget_targets_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_CACHES_BYTES",
        default_value_t = 53_687_091_200u64
    )]
    pub budget_caches_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_ARTIFACTS_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_artifacts_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_CARGO_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_cargo_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_MISE_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_mise_bytes: u64,

    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Report store sizes by store and scope. Read-only.
    Du,
    /// Preview or execute bounded cache eviction.
    Gc(CacheGcArgs),
}

#[derive(Debug, Args)]
pub struct CacheGcArgs {
    /// Print candidates without deleting anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Confirm deletion for a destructive GC run.
    #[arg(long)]
    pub yes: bool,

    /// Permit destructive GC before plan 036 lease wiring is active.
    #[arg(long)]
    pub force_no_lease_check: bool,

    /// Keep this many newest target buckets per trust/repo/workflow/job scope.
    #[arg(long, default_value_t = 3)]
    pub keep_newest_targets: usize,

    /// Consider cache/artifact/cargo/mise entries older than this many days candidates.
    #[arg(long, default_value_t = 30)]
    pub max_age_days: u64,

    /// Optional total byte ceiling for all GC-managed stores.
    #[arg(long)]
    pub max_size_bytes: Option<u64>,
}

impl From<CacheArgs> for rt::CacheArgs {
    fn from(args: CacheArgs) -> Self {
        Self {
            work_dir: args.work_dir,
            config_dir: args.config_dir,
            budget_targets_bytes: args.budget_targets_bytes,
            budget_caches_bytes: args.budget_caches_bytes,
            budget_artifacts_bytes: args.budget_artifacts_bytes,
            budget_cargo_bytes: args.budget_cargo_bytes,
            budget_mise_bytes: args.budget_mise_bytes,
            command: match args.command {
                CacheCommand::Du => rt::CacheCommand::Du,
                CacheCommand::Gc(gc) => rt::CacheCommand::Gc(rt::CacheGcArgs {
                    dry_run: gc.dry_run,
                    yes: gc.yes,
                    force_no_lease_check: gc.force_no_lease_check,
                    keep_newest_targets: gc.keep_newest_targets,
                    max_age_days: gc.max_age_days,
                    max_size_bytes: gc.max_size_bytes,
                }),
            },
        }
    }
}

#[derive(Debug, Args)]
pub struct CapabilitiesArgs {
    #[command(subcommand)]
    pub command: CapabilitiesCommand,
}

#[derive(Debug, Subcommand)]
pub enum CapabilitiesCommand {
    /// Validate a sanitized broker job-message JSON dump.
    Check { job_dump: PathBuf },
    /// Export the compiled manifest as JSON.
    Export,
}

impl From<CapabilitiesArgs> for rt::CapabilitiesArgs {
    fn from(args: CapabilitiesArgs) -> Self {
        Self {
            command: match args.command {
                CapabilitiesCommand::Check { job_dump } => {
                    rt::CapabilitiesCommand::Check { job_dump }
                }
                CapabilitiesCommand::Export => rt::CapabilitiesCommand::Export,
            },
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct ConfigureArgs {
    /// Repository, organization, or enterprise URL accepted by GitHub JIT runner configuration.
    #[arg(long)]
    pub url: String,

    /// GitHub personal access token used to create a JIT runner configuration.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Runner display name.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels for the JIT runner. Example: velnor,hetzner-sentry-ci.
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Add labels needed by the current target repositories' x64 Linux jobs.
    #[arg(long)]
    pub target_mvp_labels: bool,

    /// Add the current target repositories' ARM Linux label.
    #[arg(long)]
    pub target_mvp_arm_label: bool,

    /// Replace an existing runner with the same name.
    #[arg(long)]
    pub replace: bool,

    /// Optional runner group id. Organization/enterprise scopes require
    /// `--pool-name`/`VELNOR_POOL_NAME`; no scope may silently use group 1.
    #[arg(long)]
    pub pool_id: Option<i64>,

    /// Resolve this organization or enterprise runner group name through GitHub.
    #[arg(long)]
    pub pool_name: Option<String>,

    /// Validate local config and payloads without calling GitHub.
    #[arg(long)]
    pub dry_run: bool,

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

impl From<ConfigureArgs> for rt::ConfigureArgs {
    fn from(args: ConfigureArgs) -> Self {
        Self {
            url: args.url,
            pat: args.pat,
            name: args.name,
            labels: args.labels,
            target_mvp_labels: args.target_mvp_labels,
            target_mvp_arm_label: args.target_mvp_arm_label,
            replace: args.replace,
            pool_id: args.pool_id,
            pool_name: args.pool_name,
            pool_id_pre_resolved: false,
            dry_run: args.dry_run,
            config_dir: args.config_dir,
        }
    }
}

#[derive(Debug, Args)]
pub struct StorageArgs {
    /// Store configuration under this directory for legacy/dev-mode resolution.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: StorageCommand,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Print every resolved storage root.
    Paths,
    /// Print bytes by canonical trust scope and class.
    Status,
    /// Print canonical storage accounting.
    Du,
    /// Execute a reviewed storage GC plan.
    Gc(CacheGcArgs),
    /// Print bounded storage history.
    History,
    /// Print active storage reservations.
    Reservations,
    /// Print active storage leases.
    Leases,
    /// Explain current storage pressure.
    ExplainPressure,
}

impl From<StorageArgs> for rt::StorageArgs {
    fn from(args: StorageArgs) -> Self {
        Self {
            config_dir: args.config_dir,
            command: match args.command {
                StorageCommand::Paths => rt::StorageCommand::Paths,
                StorageCommand::Status => rt::StorageCommand::Status,
                StorageCommand::Du
                | StorageCommand::Gc(_)
                | StorageCommand::History
                | StorageCommand::Reservations
                | StorageCommand::Leases
                | StorageCommand::ExplainPressure => rt::StorageCommand::Status,
            },
        }
    }
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Repository, organization, or enterprise URL the daemon registers against.
    #[arg(long)]
    pub url: String,

    /// Runner base name (slots register as <name>-slot-N).
    #[arg(long, default_value = "velnor")]
    pub name: String,

    /// Expected number of runner slots.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// GitHub token used to list runners (same credential as the daemon).
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,
}

impl From<DoctorArgs> for rt::DoctorArgs {
    fn from(args: DoctorArgs) -> Self {
        Self {
            url: args.url,
            name: args.name,
            slots: args.slots,
            pat: args.pat,
        }
    }
}

#[derive(Debug, Args)]
pub struct PreflightArgs {
    /// Directory containing execution.toml (`[execution] backend`).
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Host work directory for Docker job state. Defaults to ./.velnor-work.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. Use when DOCKER_HOST points at a remote daemon with the work dir mounted at a different path.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Docker image used for the bind-mount visibility check.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Require docker buildx to be available on the host Docker backend.
    #[arg(long, default_value_t = true)]
    pub require_buildx: bool,
}

impl From<PreflightArgs> for rt::PreflightArgs {
    fn from(args: PreflightArgs) -> Self {
        let dir = args
            .config_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("/etc/velnor"));
        let (backend, require_docker_socket) =
            match velnor_runner::execution::load_execution_file(dir, None) {
                Ok(file) => {
                    let backend = file.backend();
                    (Some(backend), backend.uses_host_docker_socket())
                }
                Err(_) => (None, false),
            };
        Self {
            work_dir: args.work_dir,
            docker_host_work_dir: args.docker_host_work_dir,
            docker_image: args.docker_image,
            require_docker_socket,
            require_buildx: require_docker_socket && args.require_buildx,
            execution_backend: backend,
            config_dir: args.config_dir,
        }
    }
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// GitHub personal access token used to delete the exact stored JIT runner id.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Only remove local configuration, even if --pat is provided.
    #[arg(long)]
    pub local_only: bool,

    /// Number of daemon slot configs to remove. For --slots > 1, removes <config-dir>/slots/slot-N.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

impl From<RemoveArgs> for rt::RemoveArgs {
    fn from(args: RemoveArgs) -> Self {
        Self {
            pat: args.pat,
            local_only: args.local_only,
            slots: args.slots,
            config_dir: args.config_dir,
        }
    }
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Number of daemon slot configs to inspect. For --slots > 1, reads <config-dir>/slots/slot-N.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Validate that local config is ready for current target repository x64 Linux jobs.
    #[arg(long)]
    pub check_target_mvp: bool,

    /// Print the node health vector (not systemd is-active).
    #[arg(long)]
    pub json: bool,

    /// Journal directory for `--json` (journal.db + health.sock).
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
}

impl From<StatusArgs> for rt::StatusArgs {
    fn from(args: StatusArgs) -> Self {
        Self {
            config_dir: args.config_dir,
            slots: args.slots,
            check_target_mvp: args.check_target_mvp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnorctl-preflight-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn preflight_cli(config_dir: Option<PathBuf>) -> PreflightArgs {
        PreflightArgs {
            config_dir,
            work_dir: None,
            docker_host_work_dir: None,
            docker_image: "ubuntu:24.04".into(),
            require_buildx: true,
        }
    }

    #[test]
    fn invalid_execution_toml_does_not_force_host_docker_socket() {
        let dir = temp_dir("invalid");
        fs::write(dir.join("execution.toml"), "not a backend table\n").unwrap();
        let converted: rt::PreflightArgs = preflight_cli(Some(dir.clone())).into();
        assert!(converted.execution_backend.is_none());
        assert!(!converted.require_docker_socket);
        assert!(!converted.require_buildx);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn microvm_execution_toml_does_not_require_host_docker_socket() {
        let dir = temp_dir("microvm");
        fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"microvm\"\n",
        )
        .unwrap();
        let converted: rt::PreflightArgs = preflight_cli(Some(dir.clone())).into();
        assert_eq!(
            converted.execution_backend,
            Some(velnor_model::ExecutionBackendKind::MicroVm)
        );
        assert!(!converted.require_docker_socket);
        assert!(!converted.require_buildx);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn docker_execution_toml_requires_host_docker_socket() {
        let dir = temp_dir("docker");
        fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"docker\"\n",
        )
        .unwrap();
        let converted: rt::PreflightArgs = preflight_cli(Some(dir.clone())).into();
        assert_eq!(
            converted.execution_backend,
            Some(velnor_model::ExecutionBackendKind::Docker)
        );
        assert!(converted.require_docker_socket);
        assert!(converted.require_buildx);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_state_db_path_is_carried_without_environment_mutation() {
        let explicit = Path::new("/tmp/velnor-test/state.db");
        assert_eq!(resolve_state_db_path(Some(explicit)), explicit);
    }
}
