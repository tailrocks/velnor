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
    /// Validate local Docker prerequisites before polling GitHub for jobs.
    Preflight(PreflightArgs),
    /// Start polling GitHub for jobs.
    Run(RunArgs),
    /// Remove local runner configuration.
    Remove(RemoveArgs),
    /// Print local runner configuration status.
    Status(StatusArgs),
    /// Check future Pkl workflow authoring files.
    Workflow(WorkflowArgs),
}

#[derive(Debug, Args)]
pub struct PreflightArgs {
    /// Host work directory for Docker job state. Defaults to ./.velnor-work.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Docker image used for the bind-mount visibility check.
    #[arg(long, default_value = "ghcr.io/catthehacker/ubuntu:act-latest")]
    pub docker_image: String,

    /// Require /var/run/docker.sock to exist on the host.
    #[arg(long)]
    pub require_docker_socket: bool,

    /// Require docker buildx to be available on the host.
    #[arg(long, default_value_t = true)]
    pub require_buildx: bool,
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Exit after one job.
    #[arg(long)]
    pub once: bool,

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
    #[arg(long, default_value = "ghcr.io/catthehacker/ubuntu:act-latest")]
    pub docker_image: String,

    /// Override Docker image used to run JavaScript actions. By default Velnor uses the action's declared Node runtime image.
    #[arg(long, default_value = "")]
    pub node_action_image: String,

    /// Host work directory for Docker job state. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
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

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Evaluate a Pkl workflow file to JSON and run Velnor structural checks.
    Check(WorkflowCheckArgs),
}

#[derive(Debug, Args)]
pub struct WorkflowCheckArgs {
    /// Pkl workflow module to evaluate.
    pub path: PathBuf,

    /// Pkl executable to use. Defaults to pkl on PATH.
    #[arg(long, default_value = "pkl")]
    pub pkl_bin: PathBuf,
}
