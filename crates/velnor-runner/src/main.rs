mod action;
mod checkout;
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
mod sd_notify;
mod slot_log;
mod telemetry;
mod workflow_command;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Spans/events go to <config-base>/logs/trace.jsonl for the long-running
    // commands; one-shot commands only surface warnings on stderr.
    let telemetry_dir = match &cli.command {
        Command::Run(args) => config::config_dir(args.config_dir.clone())
            .ok()
            .map(|dir| dir.join("logs")),
        Command::Daemon(args) => runner::daemon_config_dir(args)
            .ok()
            .map(|dir| dir.join("logs")),
        _ => None,
    };
    telemetry::init(telemetry_dir.as_deref());

    match cli.command {
        Command::Configure(args) => runner::configure(args).await,
        Command::Daemon(args) => runner::daemon(args).await,
        Command::Preflight(args) => preflight::preflight(args),
        Command::Run(args) => runner::run(args).await,
        Command::Remove(args) => runner::remove(args).await,
        Command::Status(args) => runner::status(args).await,
        Command::Doctor(args) => runner::doctor(args).await,
    }
}
