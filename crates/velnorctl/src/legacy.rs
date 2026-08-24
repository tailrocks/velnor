//! The migrated service/operator command trees formerly parsed by
//! `crates/velnor-runner/src/cli.rs`.
//!
//! This crate owns every Velnor CLI surface (Plan 064 dependency law): the
//! types below are the single clap parsing source, and each converts
//! explicitly into the plain runtime argument types in
//! [`velnor_runner::args`] at this boundary. The interim
//! [`velnor_runner::scaffold`] helpers execute the handlers with identical
//! bootstrap behavior until Plan 079 deletes the legacy crate.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use velnor_runner::args as rt;

pub use velnor_runner::scaffold::{dispatch, enforce_admission, init_telemetry, telemetry_dir};

/// Default host location of the atomically activated release identity.
pub const ACTIVE_RELEASE_DIR: &str = rt::ACTIVE_RELEASE_DIR;

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

    /// Runner group id for JIT configuration. Defaults to GitHub's default group id 1.
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
}

impl From<StorageArgs> for rt::StorageArgs {
    fn from(args: StorageArgs) -> Self {
        Self {
            config_dir: args.config_dir,
            command: match args.command {
                StorageCommand::Paths => rt::StorageCommand::Paths,
                StorageCommand::Status => rt::StorageCommand::Status,
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
    /// Host work directory for Docker job state. Defaults to ./.velnor-work.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. Use when DOCKER_HOST points at a remote daemon with the work dir mounted at a different path.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Docker image used for the bind-mount visibility check.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Require /var/run/docker.sock to exist on the host.
    #[arg(long)]
    pub require_docker_socket: bool,

    /// Require docker buildx to be available on the host.
    #[arg(long, default_value_t = true)]
    pub require_buildx: bool,
}

impl From<PreflightArgs> for rt::PreflightArgs {
    fn from(args: PreflightArgs) -> Self {
        Self {
            work_dir: args.work_dir,
            docker_host_work_dir: args.docker_host_work_dir,
            docker_image: args.docker_image,
            require_docker_socket: args.require_docker_socket,
            require_buildx: args.require_buildx,
        }
    }
}

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    pub command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
    /// Emit a coherent release record from this (release-build) binary. Refuses
    /// to run from a development build.
    Emit(ReleaseEmitArgs),
    /// Re-assemble and re-verify a record from downloaded artifacts.
    Assemble(ReleaseAssembleArgs),
    /// Verify a release record against its independent checksum and internal
    /// coherence.
    VerifyRecord(ReleaseVerifyRecordArgs),
    /// Validate the installed binary/package/manifest against the active record.
    /// Run by both `.service` units before ExecStart.
    VerifyInstalled(ReleaseVerifyInstalledArgs),
    /// Atomically activate a record, demoting the current active to rollback.
    Activate(ReleaseActivateArgs),
    /// Restore the previous coherent record.
    Rollback(ReleaseRollbackArgs),
    /// Print this binary's embedded build identity (or a deployed identity file).
    Export(ReleaseExportArgs),
}

#[derive(Debug, Args)]
pub struct ReleaseEmitArgs {
    /// Path to the assembled release record JSON to validate + persist.
    #[arg(long)]
    pub record: PathBuf,
    /// Release store root the immutable record is written under.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub out_dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseAssembleArgs {
    /// Candidate record JSON.
    #[arg(long)]
    pub record: PathBuf,
    /// Directory of downloaded artifacts (per-arch `*.bin.sha256` sidecars) to
    /// cross-check the record's digests against.
    #[arg(long)]
    pub artifacts: Option<PathBuf>,
    /// Write the re-verified canonical record + checksum here.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseVerifyRecordArgs {
    /// Release record JSON.
    #[arg(long)]
    pub record: PathBuf,
    /// Independent checksum file (`<sha256>  <name>` format).
    #[arg(long)]
    pub checksum: Option<PathBuf>,
    /// Independent checksum as a bare 64-hex string.
    #[arg(long)]
    pub sha256: Option<String>,
    /// Optional APT publication record to cross-check binds this source record.
    #[arg(long)]
    pub publication: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseVerifyInstalledArgs {
    /// Active release record.
    #[arg(long, default_value = rt::ACTIVE_RECORD_PATH)]
    pub record: PathBuf,
    /// Active deployed-identity pointer.
    #[arg(long, default_value = rt::ACTIVE_DEPLOYED_PATH)]
    pub deployed: PathBuf,
    /// Installed binary to hash.
    #[arg(long, default_value = rt::INSTALLED_BINARY_PATH)]
    pub binary: PathBuf,
    /// Host architecture override (amd64|arm64); defaults to the running arch.
    #[arg(long)]
    pub arch: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReleaseActivateArgs {
    /// Release store root.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
    /// Record to activate.
    #[arg(long)]
    pub record: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseRollbackArgs {
    /// Release store root.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseExportArgs {
    /// Optional deployed-identity file to pretty-print instead of the embedded
    /// build identity.
    #[arg(long)]
    pub deployed: Option<PathBuf>,
}

macro_rules! forward_release_args {
    ($from:ty, $to:ty { $($field:ident),+ $(,)? }) => {
        impl From<$from> for $to {
            fn from(args: $from) -> Self {
                Self { $($field: args.$field),+ }
            }
        }
    };
}

forward_release_args!(ReleaseEmitArgs, rt::ReleaseEmitArgs { record, out_dir });
forward_release_args!(
    ReleaseAssembleArgs,
    rt::ReleaseAssembleArgs {
        record,
        artifacts,
        out
    }
);
forward_release_args!(
    ReleaseVerifyRecordArgs,
    rt::ReleaseVerifyRecordArgs {
        record,
        checksum,
        sha256,
        publication
    }
);
forward_release_args!(
    ReleaseVerifyInstalledArgs,
    rt::ReleaseVerifyInstalledArgs {
        record,
        deployed,
        binary,
        arch
    }
);
forward_release_args!(ReleaseActivateArgs, rt::ReleaseActivateArgs { dir, record });
forward_release_args!(ReleaseRollbackArgs, rt::ReleaseRollbackArgs { dir });
forward_release_args!(ReleaseExportArgs, rt::ReleaseExportArgs { deployed });

impl From<ReleaseArgs> for rt::ReleaseArgs {
    fn from(args: ReleaseArgs) -> Self {
        Self {
            command: match args.command {
                ReleaseCommand::Emit(a) => rt::ReleaseCommand::Emit(a.into()),
                ReleaseCommand::Assemble(a) => rt::ReleaseCommand::Assemble(a.into()),
                ReleaseCommand::VerifyRecord(a) => rt::ReleaseCommand::VerifyRecord(a.into()),
                ReleaseCommand::VerifyInstalled(a) => rt::ReleaseCommand::VerifyInstalled(a.into()),
                ReleaseCommand::Activate(a) => rt::ReleaseCommand::Activate(a.into()),
                ReleaseCommand::Rollback(a) => rt::ReleaseCommand::Rollback(a.into()),
                ReleaseCommand::Export(a) => rt::ReleaseCommand::Export(a.into()),
            },
        }
    }
}

/// Shared execution flags of the daemon process (service plumbing invoked by
/// systemd via `velnorctl daemon`). The standalone `run` worker is internal:
/// C075 folds it into `daemon --once`, so no CLI arm exposes it.
#[derive(Debug, Clone, Args)]
pub struct DaemonArgs {
    /// Base configuration directory. For --slots > 1, each slot reads config from <config-dir>/slots/slot-N.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Repository, organization, or enterprise URL accepted by GitHub JIT runner configuration. If provided, daemon configures internal slots before polling.
    #[arg(long)]
    pub url: Option<String>,

    /// GitHub personal access token used to create JIT runner configurations.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Base runner display name. For --slots > 1, Velnor appends -slot-N.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels for each JIT runner. Example: velnor,hetzner-sentry-ci.
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Add labels needed by the current target repositories' x64 Linux jobs.
    #[arg(long)]
    pub target_mvp_labels: bool,

    /// Add the current target repositories' ARM Linux label.
    #[arg(long)]
    pub target_mvp_arm_label: bool,

    /// Replace existing slot configs during daemon startup JIT configuration.
    #[arg(long)]
    pub replace: bool,

    /// Runner group id for JIT configuration. Defaults to GitHub's default group id 1.
    #[arg(long, env = "VELNOR_POOL_ID")]
    pub pool_id: Option<i64>,

    /// Resolve this organization or enterprise runner group name through GitHub.
    #[arg(long, env = "VELNOR_POOL_NAME")]
    pub pool_name: Option<String>,

    /// Validate daemon slot JIT payloads without calling GitHub.
    #[arg(long = "dry-run-jit-config")]
    pub dry_run_registration: bool,

    /// Number of internal GitHub runner slots managed by this daemon.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Recycle an idle slot after this many seconds even when it still looks
    /// healthy, so every slot gets a periodically fresh JIT registration.
    /// 0 disables the bound. Defaults to 14400 (4 hours).
    #[arg(long)]
    pub max_idle_slot_age_seconds: Option<u64>,

    /// Exit each internal slot after one job. Useful for bounded live proof runs.
    #[arg(long)]
    pub once: bool,

    /// Fail each slot if no job is acquired within this many seconds. Default is no idle timeout.
    #[arg(long)]
    pub idle_timeout_seconds: Option<u64>,

    /// Mark received jobs as succeeded without executing user steps.
    #[arg(long)]
    pub complete_noop: bool,

    /// Execute supported run steps in Docker, then finish and acknowledge the job. This is the default unless --complete-noop or --dry-run-jobs is set.
    #[arg(long)]
    pub execute_scripts: bool,

    /// Poll and inspect jobs without acknowledging or executing them.
    #[arg(long)]
    pub dry_run_jobs: bool,

    /// Write sanitized AgentJobRequestMessage JSON snapshots to this file or directory. For --slots > 1, Velnor writes under slot-N children.
    #[arg(long)]
    pub dump_job_message: Option<PathBuf>,

    /// Docker image for executable jobs.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Docker --cpus limit appended to every job container. Empty disables the daemon-level CPU cap.
    #[arg(long, env = "VELNOR_JOB_CPUS", default_value = "")]
    pub job_cpus: String,

    /// Docker --memory limit appended to every job container. Empty disables the daemon-level memory cap.
    #[arg(long, env = "VELNOR_JOB_MEMORY", default_value = "")]
    pub job_memory: String,

    /// Trust boundary for this daemon/pool. "trusted" keeps full capabilities; any other value disables shared Docker socket access and rejects user secrets.
    #[arg(long, env = "VELNOR_TRUST_SCOPE", default_value = "trusted")]
    pub trust_scope: String,

    /// Filesystem bytes never available to new jobs.
    #[arg(
        long,
        env = "VELNOR_EMERGENCY_RESERVE_BYTES",
        default_value_t = 10_737_418_240u64
    )]
    pub emergency_reserve_bytes: u64,

    /// Conservative disk reservation for every advertised slot.
    #[arg(
        long,
        env = "VELNOR_JOB_PEAK_BYTES",
        default_value_t = 32_212_254_720u64
    )]
    pub job_peak_bytes: u64,

    /// Override Docker image used to run JavaScript actions. By default Velnor uses the action's declared Node runtime image.
    #[arg(long, default_value = "")]
    pub node_action_image: String,

    /// Base host work directory for Docker job state. For --slots > 1, each slot uses a slot-N child.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. For --slots > 1, each slot uses a slot-N child.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Skip Docker preflight before polling GitHub for executable jobs.
    #[arg(long)]
    pub skip_preflight: bool,

    /// Require /var/run/docker.sock before polling GitHub for executable jobs.
    #[arg(long)]
    pub require_docker_socket: bool,
}

impl From<DaemonArgs> for rt::DaemonArgs {
    fn from(args: DaemonArgs) -> Self {
        Self {
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
            trust_scope: args.trust_scope,
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

/// Operator dispatch surface for the migrated commands. `execute_legacy`
/// converts it exhaustively into the runner runtime command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect Velnor's daemon-shared host cache stores.
    Cache(Box<CacheArgs>),
    /// Inspect or validate against the compiled strict capability manifest.
    Capabilities(Box<CapabilitiesArgs>),
    /// Create and store a GitHub JIT runner configuration.
    Configure(Box<ConfigureArgs>),
    /// Run one daemon process that manages one or more internal runner slots.
    Daemon(Box<DaemonArgs>),
    /// Probe GitHub for this daemon's registered runners and fail loudly when
    /// the fleet is gone (run from a systemd timer for alerting).
    Doctor(Box<DoctorArgs>),
    /// Validate local Docker prerequisites before polling GitHub for jobs.
    Preflight(Box<PreflightArgs>),
    /// Plan 010 release-coherence chain over the installed identity. Service
    /// plumbing until Plan 079 replaces it with signed apt/dpkg operations.
    Release(Box<ReleaseArgs>),
    /// Remove local runner configuration.
    Remove(Box<RemoveArgs>),
    /// Print local runner configuration status.
    Status(Box<StatusArgs>),
    /// Inspect the canonical Velnor storage layout and catalog.
    Storage(Box<StorageArgs>),
}

impl From<Command> for rt::Command {
    fn from(command: Command) -> Self {
        match command {
            Command::Cache(args) => Self::Cache((*args).into()),
            Command::Capabilities(args) => Self::Capabilities((*args).into()),
            Command::Configure(args) => Self::Configure((*args).into()),
            Command::Daemon(args) => Self::Daemon((*args).into()),
            Command::Doctor(args) => Self::Doctor((*args).into()),
            Command::Preflight(args) => Self::Preflight((*args).into()),
            Command::Release(args) => Self::Release((*args).into()),
            Command::Remove(args) => Self::Remove((*args).into()),
            Command::Status(args) => Self::Status((*args).into()),
            Command::Storage(args) => Self::Storage((*args).into()),
        }
    }
}
