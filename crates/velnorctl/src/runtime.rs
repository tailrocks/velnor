//! Typed clap argument structs for operator commands that still execute
//! through the interim `velnor-runner` facade (Plan 064 / 079).
//!
//! These types are the clap parse source. Conversion into
//! [`velnor_runner::args`] is exhaustive `From`, never a second argv walk.
//! `daemon` / `release` / `run` are not public `velnorctl` commands.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use velnor_runner::args as rt;

pub use velnor_runner::scaffold::{dispatch, enforce_admission, init_telemetry, telemetry_dir};

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
        let backend = args
            .config_dir
            .as_ref()
            .and_then(|dir| velnor_runner::execution::load_execution_file(dir, None).ok())
            .map(|file| file.backend());
        Self {
            work_dir: args.work_dir,
            docker_host_work_dir: args.docker_host_work_dir,
            docker_image: args.docker_image,
            require_docker_socket: backend
                .map(velnor_model::ExecutionBackendKind::uses_host_docker_socket)
                .unwrap_or(true),
            require_buildx: args.require_buildx,
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
