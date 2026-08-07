mod action;
mod admission;
mod attestation;
mod cache;
mod capacity;
mod checkout;
mod cli;
mod command_files;
mod compiler_cache;
mod config;
mod container;
mod executor;
mod fs_copy;
mod git_mirror;
mod github_adapter;
mod job_message;
mod manifest;
mod mise;
mod plan;
mod platform;
mod preflight;
mod protocol;
mod release;
mod runner;
mod runtime_env;
mod script_step;
mod sd_notify;
mod slot_log;
mod storage;
mod telemetry;
mod workflow_command;

use anyhow::Result;
use clap::Parser;
use std::time::Duration;

use crate::cli::{Cli, Command};

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> Result<()> {
    let runtime = build_runtime()?;
    let result = runtime.block_on(run());
    // Tokio waits forever for a started `spawn_blocking` task when Runtime is
    // dropped. A stuck Docker/curl cleanup must not hold a fully drained
    // systemd daemon (and package upgrade) for hours.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

async fn run() -> Result<()> {
    // Production admission is unconditional and immutable. Refuse to start if a
    // removed capability-bypass variable is present or a non-strict validation
    // mode is requested, and fail fast if the compiled manifest is not
    // structurally immutable. Both run before any command is dispatched.
    cli::enforce_strict_capability_env()?;
    manifest::assert_manifest_integrity()?;

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
        Command::Cache(args) => cache::run(args),
        Command::Capabilities(args) => manifest::run(args),
        Command::Configure(args) => runner::configure(args).await,
        Command::Daemon(args) => runner::daemon(args).await,
        Command::Preflight(args) => preflight::preflight(args),
        Command::Run(args) => runner::run(args).await,
        Command::Remove(args) => runner::remove(args).await,
        Command::Status(args) => runner::status(args).await,
        Command::Storage(args) => storage::run(args),
        Command::Doctor(args) => runner::doctor(args).await,
        Command::Release(args) => release::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_shutdown_does_not_wait_forever_for_blocking_work() {
        let runtime = build_runtime().unwrap();
        runtime.block_on(async {
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(30)));
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        let started = std::time::Instant::now();
        runtime.shutdown_timeout(Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
