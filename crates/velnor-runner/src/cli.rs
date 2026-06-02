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
    /// Register this machine as a GitHub self-hosted runner.
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
    #[arg(long, default_value = "velnor/job-ubuntu:24.04")]
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
    /// Repository, organization, or enterprise URL accepted by GitHub runner registration.
    #[arg(long)]
    pub url: String,

    /// GitHub runner registration token. If omitted, pass --pat to request one.
    #[arg(long, env = "VELNOR_RUNNER_TOKEN")]
    pub token: Option<String>,

    /// GitHub personal access token used to request a short-lived runner registration token.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Runner display name.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels to register. Example: velnor,hetzner-sentry-ci.
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

    /// Runner group/pool id. If omitted, Velnor chooses the default self-hosted pool.
    #[arg(long)]
    pub pool_id: Option<i64>,

    /// Runner group/pool name. Used when multiple self-hosted pools exist.
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
    #[arg(long, default_value = "velnor/job-ubuntu:24.04")]
    pub docker_image: String,

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

    /// Repository, organization, or enterprise URL accepted by GitHub runner registration. If provided, daemon configures internal slots before polling.
    #[arg(long)]
    pub url: Option<String>,

    /// GitHub runner registration token. If omitted, pass --pat to request one.
    #[arg(long, env = "VELNOR_RUNNER_TOKEN")]
    pub token: Option<String>,

    /// GitHub personal access token used to request short-lived runner registration tokens.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Base runner display name. For --slots > 1, Velnor appends -slot-N.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels to register. Example: velnor,hetzner-sentry-ci.
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Add labels needed by the current target repositories' x64 Linux jobs.
    #[arg(long)]
    pub target_mvp_labels: bool,

    /// Add the current target repositories' ARM Linux label.
    #[arg(long)]
    pub target_mvp_arm_label: bool,

    /// Replace existing slot runners with the same names during daemon startup registration.
    #[arg(long)]
    pub replace: bool,

    /// Runner group/pool id. If omitted, Velnor chooses the default self-hosted pool.
    #[arg(long)]
    pub pool_id: Option<i64>,

    /// Runner group/pool name. Used when multiple self-hosted pools exist.
    #[arg(long)]
    pub pool_name: Option<String>,

    /// Validate daemon slot registration payloads without calling GitHub.
    #[arg(long)]
    pub dry_run_registration: bool,

    /// Number of internal GitHub runner slots managed by this daemon.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

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
    #[arg(long, default_value = "velnor/job-ubuntu:24.04")]
    pub docker_image: String,

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
    /// GitHub runner remove token. If omitted, pass --pat to request one.
    #[arg(long, env = "VELNOR_RUNNER_REMOVE_TOKEN")]
    pub token: Option<String>,

    /// GitHub personal access token used to request a short-lived runner remove token.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Only remove local configuration, even if --token or --pat is provided.
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
