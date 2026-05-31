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
    /// Start polling GitHub for jobs.
    Run(RunArgs),
    /// Remove local runner configuration.
    Remove(RemoveArgs),
    /// Print local runner configuration status.
    Status(StatusArgs),
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
