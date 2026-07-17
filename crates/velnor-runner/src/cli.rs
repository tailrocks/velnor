use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "velnor-runner")]
#[command(about = "Rust GitHub self-hosted runner compatibility agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect Velnor's daemon-shared host cache stores.
    Cache(CacheArgs),
    /// Create and store a GitHub JIT runner configuration.
    Configure(ConfigureArgs),
    /// Run one daemon process that manages one or more internal runner slots.
    Daemon(DaemonArgs),
    /// Validate local Docker prerequisites before polling GitHub for jobs.
    Preflight(PreflightArgs),
    /// Start polling GitHub for jobs.
    Run(RunArgs),
    /// Remove local runner configuration.
    Remove(RemoveArgs),
    /// Print local runner configuration status.
    Status(StatusArgs),
    /// Inspect the canonical Velnor storage layout and catalog.
    Storage(StorageArgs),
    /// Probe GitHub for this daemon's registered runners and fail loudly when
    /// the fleet is gone (run from a systemd timer for alerting).
    Doctor(DoctorArgs),
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

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Host work directory that contains daemon-shared stores. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Store configuration under this directory when deriving the default work dir.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Report store sizes by store and scope. Read-only.
    Du,
    /// Preview cache eviction candidates. Destructive GC is not implemented in this spike.
    Gc(CacheGcArgs),
}

#[derive(Debug, Args)]
pub struct CacheGcArgs {
    /// Print candidates without deleting anything. Required in this spike.
    #[arg(long)]
    pub dry_run: bool,

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

    /// Runner group name is not supported by JIT setup; pass --pool-id for non-default groups.
    #[arg(long)]
    pub pool_name: Option<String>,

    /// Validate local config and payloads without calling GitHub.
    #[arg(long)]
    pub dry_run: bool,

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// GitHub token used for idle-slot registry health checks (the runner
    /// verifies its own registration stays online). Optional: without it the
    /// registry reconciler is disabled.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Recycle an idle slot after this many seconds even when it still looks
    /// healthy, so every slot gets a periodically fresh JIT registration.
    /// 0 disables the bound. Defaults to 14400 (4 hours).
    #[arg(long)]
    pub max_idle_slot_age_seconds: Option<u64>,

    /// Exit after one job.
    #[arg(long)]
    pub once: bool,

    /// Fail if no job is acquired within this many seconds. Default is no idle timeout.
    #[arg(long)]
    pub idle_timeout_seconds: Option<u64>,

    /// Mark a received job as succeeded without executing user steps.
    #[arg(long)]
    pub complete_noop: bool,

    /// Execute supported run steps in Docker, then finish and acknowledge the job. This is the default unless --complete-noop or --dry-run-jobs is set.
    #[arg(long)]
    pub execute_scripts: bool,

    /// Poll and inspect jobs without acknowledging or executing them.
    #[arg(long)]
    pub dry_run_jobs: bool,

    /// Write a sanitized AgentJobRequestMessage JSON snapshot to this file or directory.
    #[arg(long)]
    pub dump_job_message: Option<PathBuf>,

    /// Docker image for --execute-scripts jobs.
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

    /// Override Docker image used to run JavaScript actions. By default Velnor uses the action's declared Node runtime image.
    #[arg(long, default_value = "")]
    pub node_action_image: String,

    /// Host work directory for Docker job state. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. Use when DOCKER_HOST points at a remote daemon with the work dir mounted at a different path.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Skip Docker preflight before polling GitHub for executable jobs.
    #[arg(long)]
    pub skip_preflight: bool,

    /// Require /var/run/docker.sock before polling GitHub for executable jobs.
    #[arg(long)]
    pub require_docker_socket: bool,
}

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
    #[arg(long)]
    pub pool_id: Option<i64>,

    /// Runner group name is not supported by JIT setup; pass --pool-id for non-default groups.
    #[arg(long)]
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
