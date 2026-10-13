mod cli;
mod command_files;
mod config;
mod container;
mod executor;
mod github_adapter;
mod job_message;
mod plan;
mod platform;
mod preflight;
mod protocol;
mod runner;
mod runtime_env;
mod script_step;
mod workflow_command;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Configure(args) => runner::configure(args).await,
        Command::Daemon(args) => runner::daemon(args).await,
        Command::Preflight(args) => preflight::preflight(args),
        Command::Run(args) => runner::run(args).await,
        Command::Remove(args) => runner::remove(args).await,
        Command::Status(args) => runner::status(args).await,
    }
}
